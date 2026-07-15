/**
 * ImmorTerm Extension Entry Point
 *
 * Architecture: VS Code extension that provides immortal terminals via the ImmorTerm
 * Rust engine (GPU terminal + daemon). Legacy screen-session paths remain for
 * restoring sessions created by the retired C binary.
 *
 * Flow:
 * 1. Extension activates on VS Code startup (onStartupFinished)
 * 2. Detects legacy binary availability and extracts resources
 * 3. Initializes WorkspaceStorage, TerminalManager, and StatusBar
 * 4. Registers all commands and terminal event handlers
 * 5. Restores terminals from previous session (if enabled)
 *
 * Commands:
 * - immorterm.forgetTerminal: Forget current terminal (Ctrl+Shift+Q Q)
 * - immorterm.forgetAllTerminals: Forget all terminals (Ctrl+Shift+Q A)
 * - immorterm.renameTerminal: Rename terminal (Ctrl+Shift+R)
 * - immorterm.cleanupStale: Cleanup stale Screen sessions
 * - immorterm.killAllScreens: Kill all Screen sessions for project
 * - immorterm.showStatus: Show terminal status
 * - immorterm.syncNow: Sync terminal names now
 * - immorterm.enableForProject: Enable ImmorTerm for this project
 * - immorterm.disableForProject: Disable ImmorTerm for this project
 */

import * as vscode from 'vscode';
import { logger } from './utils/logger';
import { activate as activateWorkspace, InitializationResult } from './activation';
import { getTerminalsDir, getScriptsDir } from './utils/resource-extractor';
import {
  getRenamesDir as getImmorTermRenamesDir,
  getRestoreJsonPath,
  getTerminalsDir as getTerminalsDirFromConfig,
  getLogsDir as getLogsDirFromConfig,
  getPendingDir as getPendingDirFromConfig,
  setEnabledState,
  setServiceEnabled,
  setTheme as setConfigTheme,
  setConfigTerminalMode,
  getConfigTerminalMode,
  isFullProTier,
} from './utils/immorterm-config';
import { getProjectName } from './utils/process';
import { StatusBar } from './ui/status-bar';
import { TerminalManager } from './terminal/manager';
import { startShelvedReaper, stopShelvedReaper } from './terminal/shelved-reaper';
import { notifications } from './ui/notifications';
import {
  restoreTerminalsWithDelay,
  isRestoreOnStartupEnabled,
  trackTerminalName,
  checkAndSyncNameChange,
  generateNextName,
} from './terminal';
import { WorkspaceStorage } from './storage/workspace-state';
import {
  forgetTerminal,
  forgetAllTerminals,
  cleanupStaleTerminals,
  cleanupLogs,
  reconcileTerminal,
  renameTerminal,
  toggleTitleLock,
  reattachTerminal,
} from './commands';
import { shouldAutoCleanupStale, getClaudeSyncInterval, shouldClaudeAutoResume, saveDirtyWorkspaceSettings, getTerminalMode } from './utils/settings';
import { screenCommands } from './utils/screen-commands';
import { initRegistryClient, updateRegistryNameAndCommand, updateRegistryTheme, updateRegistryTitleLocked, getAllTerminalsFromRegistry, getCurrentClaudeSessionId, flushRegistryWrites, setActiveTerminal, migrateShelvedOutOfRegistry, sweepStubZombies, sweepOrphanShelvedEntries, backfillOwnerProjectFields } from './registry-client';
import { initClaudeSync, syncClaudeSessions, installStatuslineScript, disposeClaudeSync, getSessionManager } from './claude-sync';
import { ImmorTermViewProvider, VIEW_ID as IMMORTERM_VIEW_ID } from './gpu-terminal';
// DISABLED: teams feature temporarily commented out
// import { TeamViewProvider, TEAM_VIEW_ID } from './team-view';
import { getMemoryPort } from './services/memory/native-memory-manager';
import * as fs from 'fs/promises';
import * as fsSync from 'fs';
import * as path from 'path';

// Track command-initiated renames to prevent onDidChangeTerminalState from reverting
// Maps windowId -> {newName, oldVsCodeName, timestamp} - we skip sync if VS Code still has oldVsCodeName
// MEMORY FIX: Added TTL (60 seconds) to prevent unbounded growth if terminal closes abnormally
const commandRenames = new Map<string, { newName: string; oldVsCodeName: string; timestamp: number }>();
const COMMAND_RENAME_TTL_MS = 60000; // 60 seconds TTL for rename tracking

// Diagnostic logging helper — writes to .immorterm/diagnostic.log for debugging
// terminal tracking issues. Uses a persistent WriteStream for non-blocking writes
// instead of appendFileSync (which blocks the Extension Host event loop).
let diagLogStream: fsSync.WriteStream | null = null;
let diagLogFailed = false; // stop retrying if the path is invalid
function diagLog(msg: string): void {
  try {
    if (diagLogFailed) return;
    if (!diagLogStream) {
      const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
      if (!ws) return;
      const logDir = path.join(ws, '.immorterm');
      if (!fsSync.existsSync(logDir)) fsSync.mkdirSync(logDir, { recursive: true });
      const logPath = path.join(logDir, 'diagnostic.log');
      diagLogStream = fsSync.createWriteStream(logPath, { flags: 'a' });
      diagLogStream.on('error', () => {
        diagLogStream = null;
        diagLogFailed = true;
      });
    }
    diagLogStream.write(`[${new Date().toISOString()}] ${msg}\n`);
  } catch { /* ignore */ }
}

// Suppresses the profile provider during terminal restoration.
// When VS Code reloads with "ImmorTerm" as default profile, it auto-creates a terminal
// via provideTerminalProfile(). Without this guard, that phantom terminal writes a pending
// file → gets added to JSON → persists across reloads, growing by one each time.
// NOTE: Only the FIRST call is suppressed (auto-create on startup). Subsequent calls from
// Ctrl+Shift+` always create a terminal, even during restoration.
let restorationInProgress = false;
let autoCreateSuppressedOnce = false;

// Track restored terminals during grace period - skip syncs until OSC reasserts the correct name
// Maps windowId -> {jsonName, timestamp}
// MEMORY FIX: No longer holds Terminal object reference - just tracks the grace period
const restoredTerminals = new Map<string, { jsonName: string; timestamp: number }>();
const RESTORE_GRACE_PERIOD_MS = 5000; // 5 seconds for VS Code to settle

/**
 * Mark a terminal as restored - skips syncs during grace period
 * Called from restoration.ts after creating a restored terminal
 * MEMORY FIX: No longer takes Terminal reference to avoid holding objects
 */
export function markAsRestored(windowId: string, jsonName: string): void {
  restoredTerminals.set(windowId, { jsonName, timestamp: Date.now() });
  logger.debug(`Marked terminal as restored: ${windowId} "${jsonName}" (grace period ${RESTORE_GRACE_PERIOD_MS}ms)`);

  // After grace period, clean up the tracking entry
  setTimeout(() => {
    const entry = restoredTerminals.get(windowId);
    if (entry) {
      logger.debug(`Grace period ended for ${windowId} "${entry.jsonName}"`);
      restoredTerminals.delete(windowId);
    }
  }, RESTORE_GRACE_PERIOD_MS);
}

/**
 * Mark a terminal as renamed via command (called from rename command)
 * Stores both the new name we set and the VS Code name at rename time
 * Entry persists until VS Code tab updates, user manually renames, or TTL expires
 */
export function markCommandRenamed(windowId: string, newName: string, oldVsCodeName: string): void {
  commandRenames.set(windowId, { newName, oldVsCodeName, timestamp: Date.now() });

  // Auto-cleanup after TTL to prevent unbounded growth
  setTimeout(() => {
    const entry = commandRenames.get(windowId);
    if (entry && entry.newName === newName) {
      commandRenames.delete(windowId);
      logger.debug(`TTL expired for commandRename: ${windowId}`);
    }
  }, COMMAND_RENAME_TTL_MS);
}

/**
 * Check if we should skip syncing VS Code's name to storage
 * Returns true if:
 *   - Terminal is within restore grace period (VS Code assigning default names)
 *   - We recently renamed via command AND VS Code still shows old name
 * Returns false if: VS Code name actually changed (user manual rename) - we should sync
 */
function shouldSkipVsCodeSync(windowId: string, vsCodeName: string, storedName: string): boolean {
  // Check if terminal is within restore grace period
  const restored = restoredTerminals.get(windowId);
  if (restored) {
    const elapsed = Date.now() - restored.timestamp;
    if (elapsed < RESTORE_GRACE_PERIOD_MS) {
      logger.debug(`Skipping sync for restored terminal ${windowId} (grace period: ${elapsed}ms/${RESTORE_GRACE_PERIOD_MS}ms)`);
      return true;
    }
  }

  const pending = commandRenames.get(windowId);
  if (!pending) return false;

  // Case 1: VS Code finally updated to our new name - clear tracking, no sync needed
  if (vsCodeName === pending.newName) {
    logger.debug(`VS Code tab updated to command name "${pending.newName}" - clearing tracking`);
    commandRenames.delete(windowId);
    return true; // Skip sync - names already match
  }

  // Case 2: VS Code still has old name, storage has our new name - skip the revert
  if (vsCodeName === pending.oldVsCodeName && storedName === pending.newName) {
    logger.debug(`Skipping sync - command renamed to "${pending.newName}", VS Code still shows "${vsCodeName}"`);
    return true;
  }

  // Case 3: VS Code has a completely different name - user manually renamed
  // Clear tracking and let it sync
  logger.debug(`User renamed to "${vsCodeName}" - clearing tracking, allowing sync`);
  commandRenames.delete(windowId);
  return false;
}

/**
 * Sync a user-initiated terminal name change to storage, JSON, screen, and shell.
 * Called when VS Code terminal name changes (via UI rename).
 */
async function syncUserRename(
  windowId: string,
  newName: string,
  terminalManager: TerminalManager
): Promise<void> {
  await terminalManager.updateTerminalName(windowId, newName);
  updateRegistryNameAndCommand(windowId, newName);

  // Lock the title — user deliberately chose this name
  const storage = terminalManager.getStorage();
  await storage.updateTerminal(windowId, { titleLocked: true });
  updateRegistryTitleLocked(windowId, true);

  // Update screen title (clean name only - no timestamp prefix)
  const projectName = terminalManager.getProjectName();
  const sessionName = `${projectName}-${windowId}`;
  await screenCommands.setWindowTitle(sessionName, newName);

  // Set pending rename via screen environment variable (cleaner than file-based IPC)
  // The shell's precmd hook will query this via `screen -Q echo` and update IMMORTERM_BASE_NAME
  await screenCommands.setEnv(sessionName, 'IMMORTERM_PENDING_RENAME', newName);

  // Lock the title in screen env (for %L status bar indicator and shell lock sync)
  await screenCommands.setEnv(sessionName, 'IMMORTERM_TITLE_LOCKED', '1');

  logger.info('Synced user rename to storage, JSON, and screen (locked):', windowId, '->', newName);
}

/**
 * Sync a title notification from ImmorTerm C code to storage and JSON only.
 * Called when the C code writes to /tmp/immorterm-title-* file.
 *
 * IMPORTANT: Do NOT call screen -X title here! The title is already set in screen
 * by the C code. Calling screen -X title would be redundant and causes resize issues
 * because it triggers WindowChanged() which interferes with terminal output.
 */
async function syncTerminalTitle(
  windowId: string,
  newTitle: string,
  terminalManager: TerminalManager
): Promise<void> {
  const storage = terminalManager.getStorage();

  // Check if title is locked (user renamed) — don't sync OSC/Claude title changes
  const terminalState = storage.getTerminal(windowId);
  if (terminalState?.titleLocked) {
    // Title is locked in storage. The shell can unlock via `sname --unlock`,
    // which writes '__UNLOCK__' to the renames file. The title file watcher
    // handles that signal and clears the lock. No need to query screen env
    // here (screen -Q echo has the side effect of flashing the result on
    // the hardstatus line, which causes "1" to appear on the status bar).
    logger.debug(`Skipping title sync for locked terminal ${windowId}: "${newTitle}"`);
    return;
  }

  // Update storage and JSON only - screen title is already set by C code
  await storage.updateTerminal(windowId, { name: newTitle });
  updateRegistryNameAndCommand(windowId, newTitle);

  logger.info(`Synced title to storage/JSON: "${newTitle}" for window ${windowId}`);
}

/**
 * Set up file pollers for title change notifications from ImmorTerm C code.
 * C code writes to .immorterm/terminals/renames/{sessionname} when title changes.
 * Uses polling (1s interval) because fs.watch is unreliable on macOS.
 */
// setupTitleFileWatchers() removed — title files are now batch-read by batchSyncTitleFiles()
// called from the consolidated 30s sync loop.

/**
 * Gets the renames directory path for the current workspace.
 * Falls back to /tmp if no workspace folder is available.
 */
function getRenamesDir(): string {
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  if (workspaceFolder) {
    return getImmorTermRenamesDir(workspaceFolder.uri.fsPath);
  }
  // Fallback for non-workspace scenarios (shouldn't happen in practice)
  return '/tmp';
}

// Track last known title content per session to detect actual changes
const titleFileLastContent = new Map<string, string>();

/**
 * Batch-read all title rename files for tracked terminals.
 * Called from the consolidated 30s sync loop — replaces N per-session
 * FileSystemWatcher instances with a single batch read pass.
 *
 * Title files are written by the ImmorTerm C binary when it receives
 * OSC 0/2 escape sequences (e.g. from Claude Code's spinner).
 */
async function batchSyncTitleFiles(terminalManager: TerminalManager): Promise<void> {
  const renamesDir = getRenamesDir();
  if (renamesDir === '/tmp') return;

  const projectName = terminalManager.getProjectName();

  for (const terminal of terminalManager.getAllTerminals()) {
    const windowId = terminal.windowId;
    if (!windowId) continue;

    const sessionName = `${projectName}-${windowId}`;
    const titlePath = path.join(renamesDir, sessionName);

    try {
      const content = await fs.readFile(titlePath, 'utf-8');
      const newTitle = content.trim();
      if (!newTitle) continue;

      // Only process if content actually changed since last read
      const lastContent = titleFileLastContent.get(sessionName) || '';
      if (newTitle === lastContent) continue;
      titleFileLastContent.set(sessionName, newTitle);

      // Check for unlock signal from `sname --unlock`
      if (newTitle === '__UNLOCK__') {
        const storage = terminalManager.getStorage();
        const state = storage.getTerminal(windowId);
        if (state?.titleLocked) {
          logger.info(`Title unlocked via sname --unlock for ${windowId}`);
          await storage.updateTerminal(windowId, { titleLocked: false });
          updateRegistryTitleLocked(windowId, false);
          const pName = terminalManager.getProjectName();
          await screenCommands.setEnv(`${pName}-${windowId}`, 'IMMORTERM_TITLE_LOCKED', '0');
        }
        continue;
      }

      // Sync title if it differs from stored name
      const storedTerminal = terminalManager.getTerminalByWindowId(windowId);
      if (storedTerminal && storedTerminal.name !== newTitle) {
        logger.debug(`Title file changed: "${newTitle}" for window ${windowId}`);
        await syncTerminalTitle(windowId, newTitle, terminalManager);
      }
    } catch {
      // File doesn't exist or read error — ignore
    }
  }
}

/**
 * No-op stub — title file watchers were replaced by batch sync.
 * Kept for call-site compatibility (new terminal creation paths).
 */
export function addTitleFileWatcher(_windowId: string, _terminalManager: TerminalManager): void {
  // Title files are now batch-read every 30s by batchSyncTitleFiles().
  // No per-session watcher needed.
}

/**
 * Clean up title file on terminal close.
 * No watcher to dispose — batch sync handles reads.
 */
export function removeTitleFileWatcher(windowId: string, terminalManager: TerminalManager): void {
  const projectName = terminalManager.getProjectName();
  const sessionName = `${projectName}-${windowId}`;

  // Remove cached content
  titleFileLastContent.delete(sessionName);

  // Clean up the title file
  const renamesDir = getRenamesDir();
  const titlePath = renamesDir === '/tmp'
    ? `/tmp/immorterm-title-${sessionName}`
    : path.join(renamesDir, sessionName);
  fs.unlink(titlePath).catch(() => { /* ignore if doesn't exist */ });
}

/**
 * Generates a unique window ID in the same format as screen-auto
 * Format: {pid}-{8-char-random}
 */
function generateWindowId(): string {
  const pid = process.pid;
  const randomChars = Array.from({ length: 8 }, () => {
    const chars = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
    return chars.charAt(Math.floor(Math.random() * chars.length));
  }).join('');
  return `${pid}-${randomChars}`;
}

/**
 * Terminal Profile Provider for ImmorTerm
 *
 * Implements vscode.TerminalProfileProvider to create ImmorTerm terminals
 * that wrap in GNU Screen sessions for persistence.
 */
class ImmorTermProfileProvider implements vscode.TerminalProfileProvider {
  constructor(
    private readonly scriptsDir: string,
    private readonly terminalsDir: string,
    private readonly projectName: string,
    private readonly pendingDir: string,
    private readonly initReady: Promise<InitializationResult | null>
  ) {}

  async provideTerminalProfile(
    _token: vscode.CancellationToken
  ): Promise<vscode.TerminalProfile | undefined> {
    logger.info('ImmorTermProfileProvider.provideTerminalProfile() called');
    diagLog(`PROFILE_PROVIDER: called (restorationInProgress=${restorationInProgress} autoCreateSuppressedOnce=${autoCreateSuppressedOnce})`);

    // Wait for initialization to complete, with a timeout guard.
    // VS Code calls this provider both on startup (auto-create default terminal)
    // AND when the user presses Ctrl+Shift+`. We must handle both cases.
    const INIT_TIMEOUT_MS = 5_000;
    const timeout = new Promise<null>((resolve) =>
      setTimeout(() => resolve(null), INIT_TIMEOUT_MS)
    );
    const result = await Promise.race([this.initReady, timeout]);

    // FALLBACK: If init failed/timed out, try creating a terminal using pre-existing scripts.
    // The scripts were deployed to ~/.immorterm/scripts/ by a previous successful activation.
    if (!result) {
      logger.warn('ImmorTermProfileProvider: init failed or timed out — trying fallback');
      diagLog('PROFILE_PROVIDER: init failed/timed out — trying fallback');
      return this.createFallbackProfile();
    }

    // Suppress ONLY the first auto-creation during restoration (prevents phantom terminal).
    // After that, all Ctrl+Shift+` calls create terminals normally — even during restoration.
    if (restorationInProgress && !autoCreateSuppressedOnce) {
      autoCreateSuppressedOnce = true;
      logger.info('ImmorTermProfileProvider: suppressed first auto-create during restoration');
      diagLog('PROFILE_PROVIDER: suppressed first auto-create during restoration');
      return new vscode.TerminalProfile({
        shellPath: '/usr/bin/true',
        hideFromUser: true,
      });
    }

    diagLog('PROFILE_PROVIDER: creating terminal profile (normal path)');
    return this.createTerminalProfile(result.storage);
  }

  /**
   * Creates a terminal profile using fully-initialized state (normal path).
   */
  private createTerminalProfile(storage: WorkspaceStorage): vscode.TerminalProfile {
    const screenAutoPath = `${this.scriptsDir}/screen-auto`;
    const windowId = generateWindowId();
    const displayName = generateNextName(this.projectName, storage);
    logger.info('Creating new ImmorTerm terminal:', { windowId, displayName, screenAutoPath });

    this.writePendingFile(windowId, displayName);

    const screenBinary = vscode.workspace.getConfiguration('immorterm').get<string>('screenBinary', 'immorterm');

    return new vscode.TerminalProfile({
      shellPath: '/bin/bash',
      shellArgs: ['-l', '-c', screenAutoPath],
      env: {
        IMMORTERM_EXTENSION: '1',
        IMMORTERM_WINDOW_ID: windowId,
        IMMORTERM_DISPLAY_NAME: displayName,
        IMMORTERM_SCREEN_BINARY: screenBinary,
        IMMORTERM_RENAMES_DIR: path.join(this.terminalsDir, 'renames'),
      },
    });
  }

  /**
   * Creates a terminal profile using pre-existing scripts on disk.
   * Used when initialization failed/timed out but scripts exist from a previous activation.
   */
  private createFallbackProfile(): vscode.TerminalProfile | undefined {
    const screenAutoPath = `${this.scriptsDir}/screen-auto`;
    const fsSync = require('fs');

    if (!fsSync.existsSync(screenAutoPath)) {
      logger.error('ImmorTermProfileProvider fallback: screen-auto not found at', screenAutoPath);
      vscode.window.showWarningMessage(
        'ImmorTerm: Initialization timed out and scripts not deployed yet. Please reload VS Code.'
      );
      return undefined;
    }

    const windowId = generateWindowId();
    // Fallback name: derive from currently open terminals (no storage needed)
    const displayName = this.generateFallbackName();
    logger.info('Creating FALLBACK ImmorTerm terminal:', { windowId, displayName, screenAutoPath });

    this.writePendingFile(windowId, displayName);

    const screenBinary = vscode.workspace.getConfiguration('immorterm').get<string>('screenBinary', 'immorterm');

    return new vscode.TerminalProfile({
      shellPath: '/bin/bash',
      shellArgs: ['-l', '-c', screenAutoPath],
      env: {
        IMMORTERM_EXTENSION: '1',
        IMMORTERM_WINDOW_ID: windowId,
        IMMORTERM_DISPLAY_NAME: displayName,
        IMMORTERM_SCREEN_BINARY: screenBinary,
        IMMORTERM_RENAMES_DIR: path.join(this.terminalsDir, 'renames'),
      },
    });
  }

  /**
   * Generates a display name from currently open VS Code terminals (no storage needed).
   */
  private generateFallbackName(): string {
    const pattern = /^immorterm-(\d+)$/i;
    let maxN = 0;
    for (const terminal of vscode.window.terminals) {
      const match = terminal.name.match(pattern);
      if (match) {
        const n = parseInt(match[1], 10);
        if (n > maxN) maxN = n;
      }
    }
    return `immorterm-${maxN + 1}`;
  }

  /**
   * Writes a pending file for reconciliation to pick up.
   */
  private writePendingFile(windowId: string, displayName: string): void {
    const pendingFile = path.join(this.pendingDir, windowId);
    const fsSync = require('fs');
    try {
      fsSync.mkdirSync(this.pendingDir, { recursive: true });
      fsSync.writeFileSync(pendingFile, `${windowId} ${displayName}`);
    } catch (err) {
      logger.warn('Failed to write pending file:', err);
    }
  }
}

/**
 * Migrates default terminal profile settings from legacy "screen" to "immorterm.screen"
 * This handles cases where users or v2 installer set the default profile to "screen"
 */
async function migrateDefaultProfileSettings(): Promise<void> {
  const config = vscode.workspace.getConfiguration('terminal.integrated');
  const platforms = ['osx', 'linux', 'windows'] as const;

  for (const platform of platforms) {
    const settingKey = `defaultProfile.${platform}`;
    const currentValue = config.get<string>(settingKey);

    if (currentValue === 'screen') {
      try {
        // Update to the new profile title (VS Code uses title, not ID)
        await config.update(settingKey, 'ImmorTerm', vscode.ConfigurationTarget.Global);
        logger.info(`Migrated default profile setting for ${platform}: screen → ImmorTerm`);
      } catch (err) {
        logger.warn(`Failed to migrate default profile for ${platform}:`, err);
      }
    }
  }
}

// Module-level state
let initResult: InitializationResult | null = null;
let disposables: vscode.Disposable[] = [];

// Scheduled cleanup timers
let staleCleanupTimer: NodeJS.Timeout | null = null;
let logCleanupTimer: NodeJS.Timeout | null = null;
let claudeSyncTimer: NodeJS.Timeout | null = null;
let immortermProvider: ImmorTermViewProvider | null = null;
// DISABLED: teams feature temporarily commented out
// let teamViewProvider: TeamViewProvider | null = null;

// Cleanup intervals (in milliseconds)
const STALE_CLEANUP_INTERVAL = 60 * 60 * 1000; // 1 hour
const LOG_CLEANUP_INTERVAL = 10 * 60 * 1000; // 10 minutes (more frequent to prevent log bloat)

/**
 * Schedules periodic cleanup tasks
 * - Stale terminal cleanup (hourly, if autoCleanupStale is enabled)
 * - Log file cleanup (hourly)
 */
function schedulePeriodicCleanup(
  storage: import('./storage/workspace-state').WorkspaceStorage,
  logsDir: string
): void {
  // Schedule stale terminal cleanup (if enabled)
  if (shouldAutoCleanupStale()) {
    // Delay initial cleanup to let terminal restoration + screen-auto complete.
    // Cleanup archives directories of dead sessions — running too early would
    // falsely identify restoring sessions as dead (screen not yet spawned).
    const INITIAL_CLEANUP_DELAY = 60_000; // 1 minute
    setTimeout(() => {
      cleanupStaleTerminals(storage, logsDir)
        .then((result) => {
          if (result.entriesRemoved > 0 || result.sessionsArchived > 0) {
            logger.info(`Initial stale cleanup: removed ${result.entriesRemoved} entries, archived ${result.sessionsArchived} sessions`);
          }
        })
        .catch((err) => {
          logger.error('Initial stale cleanup failed:', err);
        });
    }, INITIAL_CLEANUP_DELAY);

    // Schedule periodic cleanup
    staleCleanupTimer = setInterval(async () => {
      try {
        const result = await cleanupStaleTerminals(storage, logsDir);
        if (result.entriesRemoved > 0) {
          logger.info(`Scheduled stale cleanup: removed ${result.entriesRemoved} entries`);
        }
      } catch (err) {
        logger.error('Scheduled stale cleanup failed:', err);
      }
    }, STALE_CLEANUP_INTERVAL);

    logger.debug('Scheduled stale cleanup every', STALE_CLEANUP_INTERVAL / 1000 / 60, 'minutes');
  }

  // Schedule log cleanup (always enabled)
  logCleanupTimer = setInterval(async () => {
    try {
      const result = await cleanupLogs(logsDir);
      if (result.filesRemoved > 0) {
        logger.info(`Scheduled log cleanup: removed ${result.filesRemoved} files, freed ${Math.round(result.bytesFreed / 1024)}KB`);
      }
    } catch (err) {
      logger.error('Scheduled log cleanup failed:', err);
    }
  }, LOG_CLEANUP_INTERVAL);

  logger.debug('Scheduled log cleanup every', LOG_CLEANUP_INTERVAL / 1000 / 60, 'minutes');
}

/**
 * Cancels all scheduled cleanup timers
 */
function cancelScheduledCleanup(): void {
  if (staleCleanupTimer) {
    clearInterval(staleCleanupTimer);
    staleCleanupTimer = null;
    logger.debug('Cancelled stale cleanup timer');
  }

  if (logCleanupTimer) {
    clearInterval(logCleanupTimer);
    logCleanupTimer = null;
    logger.debug('Cancelled log cleanup timer');
  }

  if (claudeSyncTimer) {
    clearInterval(claudeSyncTimer);
    claudeSyncTimer = null;
    logger.debug('Cancelled Claude sync timer');
  }

  // Clear cached title content
  titleFileLastContent.clear();
}

/**
 * Registers enable/disable commands BEFORE activateWorkspace.
 *
 * These must be available even if the Extension Host OOMs during activation.
 * Without early registration, users can't disable ImmorTerm to break the
 * OOM crash loop, and can't re-enable it after a failed activation.
 *
 * Both handlers check `initResult` (module-level) at CALL TIME, not at
 * registration time, so they adapt to whether activation succeeded or not.
 */
function registerEarlyCommands(context: vscode.ExtensionContext): void {
  // Command: Enable for This Project
  // Shows theme picker wizard, then creates .vscode/settings.json with ImmorTerm as default
  const enableForProjectCmd = vscode.commands.registerCommand(
    'immorterm.enableForProject',
    async () => {
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      if (!workspaceFolder) {
        vscode.window.showWarningMessage('ImmorTerm: No workspace folder open');
        return;
      }

      const fs = await import('fs/promises');
      const path = await import('path');
      // One terminal now — the Rust engine. Old configs may still carry 'regular'/'both'.
      const terminalModeChoice = { mode: 'ai' as const };

      // Step 1: Choose theme
      const { getThemeNames, getTheme, generateHardstatus, themeLabels } = await import('./themes');

      const themeNames = getThemeNames();
      const themeItems = themeNames.map(name => ({
        label: themeLabels[name] || name,
        description: name === 'Purple Haze' ? '(default)' : undefined,
        themeName: name,
      }));

      const selectedTheme = await vscode.window.showQuickPick(themeItems, {
        placeHolder: 'Select a status bar theme for ImmorTerm',
        title: 'ImmorTerm Setup - Choose Theme',
      });

      if (!selectedTheme) {
        // User cancelled
        return;
      }

      // Step 2: Ask about memory services (just memory, not graph)
      const memoryChoice = await vscode.window.showQuickPick(
        [
          {
            label: '$(database) Enable Persistent Memory',
            description: 'Recommended',
            detail: 'Claude remembers decisions, context, and learnings across sessions.',
            enableMemory: true,
          },
          {
            label: '$(circle-slash) Skip Memory',
            description: 'Terminals only',
            detail: 'Just use ImmorTerm for persistent terminals without memory features.',
            enableMemory: false,
          },
        ],
        {
          placeHolder: 'Enable persistent memory for Claude?',
          title: 'ImmorTerm Setup - Memory Services',
          ignoreFocusOut: true,
        }
      );

      if (!memoryChoice) {
        // User cancelled
        return;
      }

      // Step 3: Ask about MCP Gateway
      const gatewayChoice = await vscode.window.showQuickPick(
        [
          {
            label: '$(zap) Enable MCP Gateway',
            description: 'Recommended — saves ~10 GB RAM',
            detail: 'Runs all MCP servers (context7, tavily, etc.) as shared processes instead of per-session. ' +
                    'Currently using ~' + Math.round(11.7) + ' GB for MCP processes. Gateway reduces this to ~1 GB.',
            enableGateway: true,
          },
          {
            label: '$(circle-slash) Skip Gateway',
            description: 'Each Claude session spawns its own MCP servers',
            detail: 'MCP servers run per-session as usual. Uses more memory but requires no gateway process.',
            enableGateway: false,
          },
        ],
        {
          placeHolder: 'Enable MCP Gateway to reduce memory by ~90%?',
          title: 'ImmorTerm Setup - MCP Gateway',
          ignoreFocusOut: true,
        }
      );

      if (!gatewayChoice) {
        // User cancelled
        return;
      }

      // Step 4: License key (optional)
      const licenseChoice = await vscode.window.showQuickPick(
        [
          {
            label: '$(key) Yes, enter my license key',
            description: 'Activate Pro features',
            detail: 'Unlock AI Terminal, theme customization, and advanced features.',
            hasKey: true,
          },
          {
            label: '$(dash) Skip — continue with Free tier',
            description: '',
            detail: 'You can activate a license later via the command palette.',
            hasKey: false,
          },
        ],
        {
          placeHolder: 'Do you have a license key?',
          title: 'ImmorTerm Setup - License',
          ignoreFocusOut: true,
        }
      );

      if (!licenseChoice) {
        // User cancelled
        return;
      }

      let licenseKey: string | null = null;
      if (licenseChoice.hasKey) {
        const key = await vscode.window.showInputBox({
          prompt: 'Enter your ImmorTerm license key',
          placeHolder: 'XXXX-XXXX-XXXX-XXXX',
          title: 'ImmorTerm Setup - Enter License Key',
          ignoreFocusOut: true,
        });
        if (key === undefined) {
          // User cancelled
          return;
        }
        licenseKey = key || null;
      }

      const { IMMORTERM_SCRIPTS_DIR: scriptsDir2, getProjectDir: getProjectDirFn2, getProjectScreenrcPath } = await import('./utils/immorterm-config');
      const immortermDir = getProjectDirFn2(workspaceFolder.uri.fsPath);
      const vscodeDir = path.join(workspaceFolder.uri.fsPath, '.vscode');
      const screenrcPath = getProjectScreenrcPath(workspaceFolder.uri.fsPath);
      const templatePath = path.join(context.extensionPath, 'resources', 'screenrc.template');

      try {
        // Ensure directories exist
        await fs.mkdir(vscodeDir, { recursive: true });
        await fs.mkdir(immortermDir, { recursive: true });
        await fs.mkdir(scriptsDir2, { recursive: true });

        // Save dirty settings.json first — VS Code refuses config writes on dirty files.
        await saveDirtyWorkspaceSettings();

        // Write all settings via VS Code config API (not fs.writeFile!)
        // Using the config API ensures in-memory cache stays in sync with the file.
        const immortermConfig = vscode.workspace.getConfiguration('immorterm');
        const terminalConfig = vscode.workspace.getConfiguration('terminal.integrated');

        await immortermConfig.update('enabled', true, vscode.ConfigurationTarget.Workspace);
        await immortermConfig.update('statusBarTheme', selectedTheme.themeName, vscode.ConfigurationTarget.Workspace);

        // Write-through to config.json
        const { getStableProjectId: getProjectIdFn } = await import('./services/memory');
        const projectId = getProjectIdFn(workspaceFolder.uri.fsPath);
        setEnabledState(workspaceFolder.uri.fsPath, true, projectId);
        setConfigTheme(workspaceFolder.uri.fsPath, selectedTheme.themeName, projectId);
        await immortermConfig.update('terminalMode', terminalModeChoice.mode, vscode.ConfigurationTarget.Workspace);
        setConfigTerminalMode(workspaceFolder.uri.fsPath, terminalModeChoice.mode, projectId);
        await terminalConfig.update('defaultProfile.osx', 'ImmorTerm', vscode.ConfigurationTarget.Workspace);
        await terminalConfig.update('defaultProfile.linux', 'ImmorTerm', vscode.ConfigurationTarget.Workspace);
        // Disable VS Code's automatic contrast adjustment for terminals.
        // The status bar gradient uses carefully chosen theme colors; VS Code's
        // minimumContrastRatio (default 4.5) overrides foreground colors on bright
        // gradient backgrounds, turning white text to black for "accessibility".
        await terminalConfig.update('minimumContrastRatio', 1, vscode.ConfigurationTarget.Workspace);
        // Disable GPU-accelerated terminal rendering (WebGL).
        // ImmorTerm's truecolor gradient, shimmer, and animated dot rendering produces
        // complex SGR escape sequences that cause character distortion under xterm.js's
        // WebGL renderer. The canvas renderer handles these correctly.
        await terminalConfig.update('gpuAcceleration', 'off', vscode.ConfigurationTarget.Workspace);

        // Prevent Biome JSONC crash: Biome doesn't support JSONC formatting, but
        // VS Code treats settings.json as JSONC. When we write theme settings,
        // VS Code asks the default formatter to range-format the change. If Biome
        // is the default formatter, this crashes the Biome LSP server.
        // Only set the override if Biome is the current default formatter.
        const editorConfig = vscode.workspace.getConfiguration('editor');
        const defaultFormatter = editorConfig.get<string>('defaultFormatter', '');
        if (defaultFormatter === 'biomejs.biome') {
          const jsoncEditorConfig = vscode.workspace.getConfiguration('editor', { languageId: 'jsonc' });
          const jsoncFormatter = jsoncEditorConfig.inspect<string>('defaultFormatter');
          if (!jsoncFormatter?.workspaceValue) {
            await jsoncEditorConfig.update('defaultFormatter', 'vscode.json-language-features', vscode.ConfigurationTarget.Workspace);
          }
        }

        // Deploy scripts (screen-auto, screenrc, shell config) to ~/.immorterm/scripts/
        // and create per-project directories. Must happen after settings are written
        // so extractScreenrcWithTheme() reads the correct theme from VS Code config.
        const { extractResources: extractResourcesFn } = await import('./utils/resource-extractor');
        extractResourcesFn(context, workspaceFolder);
        logger.info('Deployed scripts to', scriptsDir2);

        // Apply theme to screenrc - preserve existing customizations if file exists
        try {
          let baseContent: string;
          try {
            // Try to read existing screenrc first to preserve customizations
            baseContent = await fs.readFile(screenrcPath, 'utf-8');
            logger.info('Using existing screenrc as base for theme application');
          } catch {
            // screenrc doesn't exist yet, use template
            baseContent = await fs.readFile(templatePath, 'utf-8');
            logger.info('Using template as base for theme application');
          }
          const theme = getTheme(selectedTheme.themeName);
          const themedHardstatus = `hardstatus alwayslastline ${generateHardstatus(theme)}`;
          const themedContent = baseContent.replace(
            /^hardstatus alwayslastline .+$/m,
            themedHardstatus
          );
          await fs.writeFile(screenrcPath, themedContent, 'utf-8');
          logger.info('Applied theme to screenrc:', selectedTheme.themeName);
        } catch (err) {
          // Template might not exist yet, that's okay - it will be created on first terminal
          logger.warn('Could not apply theme to screenrc:', err);
        }

        // Enable/disable memory based on user choice
        if (memoryChoice.enableMemory) {
          await immortermConfig.update('services.memory.enabled', true, vscode.ConfigurationTarget.Workspace);
          setServiceEnabled(workspaceFolder.uri.fsPath, 'memory', true, projectId);

          // Install memory hooks
          const { getStableProjectId, installMemoryHooks, validateAndPrompt } = await import('./services/memory');
          const prerequisitesMet = await validateAndPrompt();
          if (prerequisitesMet) {
            const memProjectId = getStableProjectId(workspaceFolder.uri.fsPath);
            installMemoryHooks(workspaceFolder.uri.fsPath, memProjectId);
            logger.info('Memory enabled and hooks installed for project:', memProjectId);
          }
        } else {
          await immortermConfig.update('services.memory.enabled', false, vscode.ConfigurationTarget.Workspace);
          setServiceEnabled(workspaceFolder.uri.fsPath, 'memory', false, projectId);
        }

        // Enable/disable MCP gateway based on user choice
        if (gatewayChoice.enableGateway) {
          await immortermConfig.update('services.mcpGateway.enabled', true, vscode.ConfigurationTarget.Workspace);
          setServiceEnabled(workspaceFolder.uri.fsPath, 'mcpGateway', true, projectId);
          logger.info('MCP Gateway enabled for project');

          // Kill old per-session MCP processes to force immediate reconnection through gateway.
          // This frees ~10 GB RAM immediately instead of waiting for sessions to cycle.
          const { startGateway, killOldMcpProcesses } = await import('./services/mcp-gateway');
          const gwState = await startGateway();
          if (gwState.healthy) {
            await killOldMcpProcesses();
            logger.info('MCP Gateway started and old MCP processes killed');
          }
        } else {
          await immortermConfig.update('services.mcpGateway.enabled', false, vscode.ConfigurationTarget.Workspace);
          setServiceEnabled(workspaceFolder.uri.fsPath, 'mcpGateway', false, projectId);
        }

        // Save license key to global config if provided
        if (licenseKey) {
          const { readGlobalConfig, writeGlobalConfig } = await import('./utils/immorterm-config');
          const globalConfig = readGlobalConfig();
          globalConfig.license.key = licenseKey;
          writeGlobalConfig(globalConfig);
        }

        const memoryMsg = memoryChoice.enableMemory ? ' + Memory' : '';
        const gatewayMsg = gatewayChoice.enableGateway ? ' + MCP Gateway' : '';
        const licenseMsg = licenseKey ? ' + License' : '';
        vscode.window.showInformationMessage(
          `ImmorTerm enabled with "${selectedTheme.themeName}" theme${memoryMsg}${gatewayMsg}${licenseMsg}! Open a new terminal to start.`,
          'Reload Window'
        ).then((selection) => {
          if (selection === 'Reload Window') {
            vscode.commands.executeCommand('workbench.action.reloadWindow');
          }
        });

        logger.info('Enabled ImmorTerm for project:', workspaceFolder.name, 'theme:', selectedTheme.themeName, 'mode:', terminalModeChoice.mode, 'memory:', memoryChoice.enableMemory, 'gateway:', gatewayChoice.enableGateway, 'license:', !!licenseKey);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: Failed to enable - ${message}`);
        logger.error('Failed to enable for project:', err);
      }
    }
  );
  context.subscriptions.push(enableForProjectCmd);

  // Command: Disable for This Project
  // Removes ImmorTerm settings and cleans up all installed files.
  // When initResult is null (activation crashed), does "lite" cleanup:
  // skips terminal kill + tab close, still removes files and settings.
  const disableForProjectCmd = vscode.commands.registerCommand(
    'immorterm.disableForProject',
    async () => {
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      if (!workspaceFolder) {
        vscode.window.showWarningMessage('ImmorTerm: No workspace folder open');
        return;
      }

      const fs = await import('fs/promises');
      const path = await import('path');
      const { getProjectDir: getProjectDirFn, getTerminalsDir: getTerminalsDirFn } = await import('./utils/immorterm-config');
      const workspacePath = workspaceFolder.uri.fsPath;
      const settingsPath = path.join(workspacePath, '.vscode', 'settings.json');
      const immortermDir = getProjectDirFn(workspacePath);
      const claudeHooksDir = path.join(workspacePath, '.claude', 'hooks');

      // Confirm with user before cleanup
      const confirm = await vscode.window.showWarningMessage(
        'ImmorTerm: This will close ALL ImmorTerm terminals for this project and remove all files (terminals directory, logs, memory hooks, settings). Continue?',
        { modal: true },
        'Yes, Disable',
        'Cancel'
      );

      if (confirm !== 'Yes, Disable') {
        return;
      }

      const cleanupResults: string[] = [];

      try {
        // 0. Kill all screen sessions and clear storage (only if activation succeeded)
        if (initResult) {
          const storage = initResult.terminalManager.getStorage();
          const forgetResult = await forgetAllTerminals(storage, initResult.logsDir, {
            skipConfirmation: true, // Already confirmed above
            showNotification: false, // We'll show our own summary
          });
          if (forgetResult.sessionsKilled > 0) {
            cleanupResults.push(`${forgetResult.sessionsKilled} terminals`);
            logger.info(`Closed ${forgetResult.sessionsKilled} ImmorTerm terminals`);
          }

          // 0b. Close VS Code terminal tabs owned by ImmorTerm
          let tabsClosed = 0;
          for (const terminal of vscode.window.terminals) {
            if (initResult.terminalManager.getWindowIdForTerminal(terminal)) {
              terminal.dispose();
              tabsClosed++;
            }
          }
          if (tabsClosed > 0) {
            logger.info(`Closed ${tabsClosed} VS Code terminal tab(s)`);
          }
        } else {
          logger.info('Disable: skipping terminal cleanup (activation did not complete)');
        }

        // 1. Remove .immorterm/ directory (terminals, config, restore JSON)
        try {
          await fs.rm(immortermDir, { recursive: true, force: true });
          cleanupResults.push('.immorterm directory');
          logger.info('Removed .immorterm directory:', immortermDir);
        } catch (err) {
          if ((err as NodeJS.ErrnoException).code !== 'ENOENT') {
            logger.warn('Failed to remove .immorterm directory:', err);
          }
        }

        // 2. Remove OpenMemory MCP server entry from .mcp.json
        try {
          const { removeOpenMemoryMCP, removeTerminalMCP } = await import('./services/memory/mcp-configurator');
          removeOpenMemoryMCP(workspacePath);
          removeTerminalMCP(workspacePath);
          cleanupResults.push('MCP config');
          logger.info('Removed OpenMemory + terminal MCP config from .mcp.json');
        } catch (err) {
          logger.warn('Failed to remove OpenMemory MCP config:', err);
        }

        // 3. Remove memory hooks from .claude/hooks/ and hooks.json
        try {
          const { removeMemoryHooks } = await import('./services/memory');
          if (removeMemoryHooks(workspacePath)) {
            cleanupResults.push('memory hooks');
            logger.info('Removed memory hooks from:', claudeHooksDir);
          }
        } catch (err) {
          logger.warn('Failed to remove memory hooks:', err);
        }

        // 3b. Stop MCP gateway and restore original configs
        try {
          const { isGatewayEnabled, stopGateway } = await import('./services/mcp-gateway');
          if (isGatewayEnabled()) {
            await stopGateway();
            cleanupResults.push('MCP gateway');
            logger.info('Stopped MCP gateway and restored config');
          }
        } catch (err) {
          logger.warn('Failed to stop MCP gateway:', err);
        }

        // 4. Remove ImmorTerm settings via VS Code config API
        try {
          await saveDirtyWorkspaceSettings();

          const config = vscode.workspace.getConfiguration();

          // Remove ImmorTerm terminal profile settings
          const osxProfile = config.inspect<string>('terminal.integrated.defaultProfile.osx');
          if (osxProfile?.workspaceValue === 'ImmorTerm') {
            await config.update('terminal.integrated.defaultProfile.osx', undefined, vscode.ConfigurationTarget.Workspace);
          }
          const linuxProfile = config.inspect<string>('terminal.integrated.defaultProfile.linux');
          if (linuxProfile?.workspaceValue === 'ImmorTerm') {
            await config.update('terminal.integrated.defaultProfile.linux', undefined, vscode.ConfigurationTarget.Workspace);
          }

          // Restore VS Code's default minimum contrast ratio
          const contrastRatio = config.inspect<number>('terminal.integrated.minimumContrastRatio');
          if (contrastRatio?.workspaceValue === 1) {
            await config.update('terminal.integrated.minimumContrastRatio', undefined, vscode.ConfigurationTarget.Workspace);
          }

          // Restore VS Code's default GPU acceleration
          const gpuAccel = config.inspect<string>('terminal.integrated.gpuAcceleration');
          if (gpuAccel?.workspaceValue === 'off') {
            await config.update('terminal.integrated.gpuAcceleration', undefined, vscode.ConfigurationTarget.Workspace);
          }

          // Restore Biome JSONC formatter override
          const jsoncEditorConfig = vscode.workspace.getConfiguration('editor', { languageId: 'jsonc' });
          const jsoncFormatter = jsoncEditorConfig.inspect<string>('defaultFormatter');
          if (jsoncFormatter?.workspaceValue === 'vscode.json-language-features') {
            await jsoncEditorConfig.update('defaultFormatter', undefined, vscode.ConfigurationTarget.Workspace);
          }

          // Remove all immorterm.* settings
          try {
            const content = await fs.readFile(settingsPath, 'utf-8');
            const fileSettings: Record<string, unknown> = JSON.parse(content);
            const immortermKeys = Object.keys(fileSettings).filter(k => k.startsWith('immorterm.'));
            for (const key of immortermKeys) {
              await config.update(key, undefined, vscode.ConfigurationTarget.Workspace);
            }
          } catch (readErr) {
            if ((readErr as NodeJS.ErrnoException).code !== 'ENOENT') {
              logger.warn('Failed to read settings for cleanup:', readErr);
            }
          }

          // Mark as explicitly disabled so activation doesn't re-trigger setup
          await config.update('immorterm.enabled', false, vscode.ConfigurationTarget.Workspace);

          cleanupResults.push('settings');
        } catch (err) {
          logger.warn('Failed to clear settings via config API:', err);
        }

        const cleanupMsg = cleanupResults.length > 0
          ? `Cleaned up: ${cleanupResults.join(', ')}`
          : 'No files to clean up';

        vscode.window.showInformationMessage(
          `ImmorTerm: Disabled for this project. ${cleanupMsg}`,
          'Reload Window'
        ).then((selection) => {
          if (selection === 'Reload Window') {
            vscode.commands.executeCommand('workbench.action.reloadWindow');
          }
        });

        logger.info('Disabled ImmorTerm for project:', workspaceFolder.name, cleanupResults);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: Failed to disable - ${message}`);
        logger.error('Failed to disable for project:', err);
      }
    }
  );
  context.subscriptions.push(disableForProjectCmd);

  // Command: New ImmorTerm Terminal (Ctrl+Shift+`)
  // Registered early so it works even if activateWorkspace() is slow or crashes.
  // Bypasses VS Code's profile provider mechanism entirely for reliability.
  const newTerminalEarlyCmd = vscode.commands.registerCommand(
    'immorterm.newTerminal',
    async () => {
      diagLog('CMD: immorterm.newTerminal triggered (early command)');

      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      if (!workspaceFolder) {
        vscode.window.showWarningMessage('ImmorTerm: No workspace folder open');
        return;
      }

      const scriptsDir = getScriptsDir();
      const screenAutoPath = `${scriptsDir}/screen-auto`;

      // Check screen-auto exists (deployed by a previous activation)
      if (!fsSync.existsSync(screenAutoPath)) {
        diagLog('CMD: immorterm.newTerminal — screen-auto not found, falling back to workbench.action.terminal.new');
        vscode.commands.executeCommand('workbench.action.terminal.new');
        return;
      }

      const terminalsDir = getTerminalsDir(workspaceFolder);
      const projectName = getProjectName(workspaceFolder);
      const windowId = generateWindowId();

      // Generate display name: use storage if init is done, otherwise count open terminals
      let displayName: string;
      if (initResult) {
        displayName = generateNextName(projectName, initResult.storage);
      } else {
        // Fallback: count existing ImmorTerm terminals
        const pattern = /^immorterm-(\d+)$/i;
        let maxN = 0;
        for (const t of vscode.window.terminals) {
          const match = t.name.match(pattern);
          if (match) {
            const n = parseInt(match[1], 10);
            if (n > maxN) maxN = n;
          }
        }
        displayName = `immorterm-${maxN + 1}`;
      }

      diagLog(`CMD: immorterm.newTerminal creating windowId=${windowId} name="${displayName}"`);

      // Write pending file for reconciliation
      const pendingDir = getPendingDirFromConfig(workspaceFolder.uri.fsPath);
      try {
        fsSync.mkdirSync(pendingDir, { recursive: true });
        fsSync.writeFileSync(path.join(pendingDir, windowId), `${windowId} ${displayName}`);
      } catch (err) {
        logger.warn('Failed to write pending file:', err);
      }

      const screenBinary = vscode.workspace.getConfiguration('immorterm').get<string>('screenBinary', 'immorterm');

      // Don't set TerminalOptions.name — omitting it allows OSC title sequences
      // from Claude Code to show in the VS Code tab (via ${sequence} template).
      // screen-auto sends an immediate OSC 0 with DISPLAY_NAME, so the tab
      // shows the correct name within milliseconds of creation.
      const terminal = vscode.window.createTerminal({
        shellPath: '/bin/bash',
        shellArgs: ['-l', '-c', screenAutoPath],
        env: {
          IMMORTERM_EXTENSION: '1',
          IMMORTERM_WINDOW_ID: windowId,
          IMMORTERM_DISPLAY_NAME: displayName,
          IMMORTERM_SCREEN_BINARY: screenBinary,
          IMMORTERM_RENAMES_DIR: path.join(terminalsDir, 'renames'),
        },
      });
      terminal.show(false);
    }
  );
  context.subscriptions.push(newTerminalEarlyCmd);
}

/**
 * Registers all ImmorTerm commands (except enable/disable, which are registered early)
 */
function registerCommands(
  context: vscode.ExtensionContext,
  terminalManager: TerminalManager,
  statusBar: StatusBar,
  logsDir: string
): void {
  // Command: New ImmorTerm Screen Terminal (Ctrl+Shift+`)
  // Bypasses VS Code's default profile mechanism which can fail at high RSS.
  // Creates an ImmorTerm terminal directly via createTerminal().
  const newScreenTerminalCmd = vscode.commands.registerCommand(
    'immorterm.newScreenTerminal',
    async () => {
      diagLog('CMD: immorterm.newScreenTerminal triggered');

      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      if (!workspaceFolder) {
        vscode.window.showWarningMessage('ImmorTerm: No workspace folder open');
        return;
      }

      const scriptsDir = getScriptsDir();
      const screenAutoPath = `${scriptsDir}/screen-auto`;
      const terminalsDir = getTerminalsDir(workspaceFolder);
      const projectName = getProjectName(workspaceFolder);
      const windowId = generateWindowId();
      const displayName = generateNextName(projectName, terminalManager.getStorage());

      diagLog(`CMD: creating terminal windowId=${windowId} name="${displayName}"`);

      // Write pending file for reconciliation
      const pendingDir = getPendingDirFromConfig(workspaceFolder.uri.fsPath);
      try {
        fsSync.mkdirSync(pendingDir, { recursive: true });
        fsSync.writeFileSync(path.join(pendingDir, windowId), `${windowId} ${displayName}`);
      } catch (err) {
        logger.warn('Failed to write pending file:', err);
      }

      const screenBinary = vscode.workspace.getConfiguration('immorterm').get<string>('screenBinary', 'immorterm');

      // Don't set TerminalOptions.name — allows OSC title sequences to show
      // in tab via ${sequence} template. screen-auto sends immediate OSC 0.
      const terminal = vscode.window.createTerminal({
        shellPath: '/bin/bash',
        shellArgs: ['-l', '-c', screenAutoPath],
        env: {
          IMMORTERM_EXTENSION: '1',
          IMMORTERM_WINDOW_ID: windowId,
          IMMORTERM_DISPLAY_NAME: displayName,
          IMMORTERM_SCREEN_BINARY: screenBinary,
          IMMORTERM_RENAMES_DIR: path.join(terminalsDir, 'renames'),
        },
      });
      terminal.show(false); // Show and focus the terminal panel
    }
  );
  context.subscriptions.push(newScreenTerminalCmd);

  // Command: Forget Current Terminal (Ctrl+Shift+Q Q)
  // T030: Gets active terminal, extracts windowId, calls forgetTerminal()
  const forgetTerminalCmd = vscode.commands.registerCommand(
    'immorterm.forgetTerminal',
    async () => {
      const activeTerminal = vscode.window.activeTerminal;
      if (!activeTerminal) {
        vscode.window.showWarningMessage('ImmorTerm: No active terminal');
        return;
      }

      const windowId = terminalManager.getWindowIdForTerminal(activeTerminal);
      if (!windowId) {
        vscode.window.showWarningMessage(
          'ImmorTerm: Current terminal is not tracked'
        );
        return;
      }

      // Use the forgetTerminal command function from commands module
      const storage = terminalManager.getStorage();
      const result = await forgetTerminal(windowId, storage, logsDir, {
        showNotification: true,
        closeTerminalTab: true,
      });

      await statusBar.update();

      logger.info('Forgot terminal:', windowId, result);
    }
  );
  context.subscriptions.push(forgetTerminalCmd);

  // Command: Forget All Terminals (Ctrl+Shift+Q A)
  // T031: Calls forgetAllTerminals() with confirmation prompt
  const forgetAllTerminalsCmd = vscode.commands.registerCommand(
    'immorterm.forgetAllTerminals',
    async () => {
      const terminals = terminalManager.getAllTerminals();
      if (terminals.length === 0) {
        vscode.window.showInformationMessage('ImmorTerm: No terminals to forget');
        return;
      }

      // Use the forgetAllTerminals command function from commands module
      // It handles confirmation internally
      const storage = terminalManager.getStorage();
      const result = await forgetAllTerminals(storage, logsDir, {
        skipConfirmation: false,
        showNotification: true,
      });

      if (result.confirmed) {
        // Close VS Code terminal tabs (command function handles Screen sessions and storage)
        for (const terminal of vscode.window.terminals) {
          terminal.dispose();
        }

        await statusBar.update();
        logger.info('Forgot all terminals:', result);
      }
    }
  );
  context.subscriptions.push(forgetAllTerminalsCmd);

  // Command: Cleanup Stale Sessions
  // T032: Calls cleanupStaleTerminals() and shows notification with cleanup count
  const cleanupStaleCmd = vscode.commands.registerCommand(
    'immorterm.cleanupStale',
    async () => {
      const storage = terminalManager.getStorage();
      const result = await cleanupStaleTerminals(storage, logsDir);

      // Show notification with cleanup count
      notifications.showCleanupComplete(result.entriesRemoved);
      await statusBar.update();

      logger.info('Cleanup stale sessions complete:', result);
    }
  );
  context.subscriptions.push(cleanupStaleCmd);

  // Command: Kill All Screen Sessions (alias for forgetAllTerminals for backward compatibility)
  const killAllScreensCmd = vscode.commands.registerCommand(
    'immorterm.killAllScreens',
    async () => {
      // Delegate to forgetAllTerminals - they now do the same thing
      await vscode.commands.executeCommand('immorterm.forgetAllTerminals');
    }
  );
  context.subscriptions.push(killAllScreensCmd);

  // Command: Show Status
  // T034: Shows QuickPick with terminal list and status
  // Includes Screen session count, total log size, storage version
  const showStatusCmd = vscode.commands.registerCommand(
    'immorterm.showStatus',
    async () => {
      const terminals = terminalManager.getAllTerminals();
      const projectName = terminalManager.getProjectName();
      const screenAvailable = statusBar.isScreenAvailable();
      const storage = terminalManager.getStorage();

      // Get Screen session count
      let screenSessionCount = 0;
      try {
        const { screenCommands } = await import('./utils/screen-commands');
        const sessions = await screenCommands.listProjectSessions(projectName);
        screenSessionCount = sessions.length;
      } catch {
        // Ignore errors in session count
      }

      // Get total log size
      let totalLogSizeMb = 0;
      try {
        const fs = await import('fs/promises');
        const path = await import('path');
        const logFiles = await fs.readdir(logsDir).catch(() => []);
        for (const logFile of logFiles) {
          if (logFile.endsWith('.log') && logFile.startsWith(`${projectName}-`)) {
            const logPath = path.join(logsDir, logFile);
            const stats = await fs.stat(logPath).catch(() => null);
            if (stats) {
              totalLogSizeMb += stats.size / (1024 * 1024);
            }
          }
        }
      } catch {
        // Ignore errors in log size calculation
      }

      // Get storage version
      const storageState = storage.getState();
      const storageVersion = storageState.version;

      // Get memory service status
      let memoryEnabled = false;
      let memoryStatus = '';
      let memoryDetail = '';
      try {
        const { isMemoryEnabled, refreshOpenMemoryState } = await import('./services/memory');
        memoryEnabled = isMemoryEnabled();
        if (memoryEnabled) {
          const state = await refreshOpenMemoryState();
          memoryStatus = state.apiHealthy ? 'Active' : state.stackRunning ? 'Starting' : 'Stopped';
          memoryDetail = `OpenMemory: ${state.apiHealthy ? '✓' : '✗'} | Sessions: ${state.activeClaudeSessions.size}`;
        }
      } catch {
        // Memory services not available
      }

      // Build status items for QuickPick
      const items: vscode.QuickPickItem[] = [
        {
          label: '$(info) Status',
          description: screenAvailable ? 'Legacy sessions available' : '',
          detail: `Project: ${projectName} | Storage Version: ${storageVersion}`,
        },
        {
          label: '$(list-unordered) Terminals',
          description: `${terminals.length} registered`,
          detail: `ImmorTerm Sessions: ${screenSessionCount} | Log Size: ${totalLogSizeMb.toFixed(2)} MB`,
        },
        {
          label: '🧠 Memory Services',
          description: memoryEnabled ? memoryStatus : 'Disabled',
          detail: memoryEnabled ? memoryDetail : 'Run "ImmorTerm: Configure Memory Services" to enable',
        },
      ];

      // Add MCP Gateway status
      try {
        const { isGatewayEnabled, getMCPGatewayState } = await import('./services/mcp-gateway');
        const gwEnabled = isGatewayEnabled();
        const gwState = getMCPGatewayState();
        const gwStatus = gwState.healthy ? 'Active' : gwState.running ? 'Starting' : 'Stopped';
        const gwDetail = gwState.healthy
          ? `Servers: ${gwState.serverCount ?? 0} | Children: ${gwState.activeChildren ?? 0} | Memory: ${gwState.memoryMB ?? 0} MB`
          : gwState.lastError
            ? `Error: ${gwState.lastError}`
            : 'Run "ImmorTerm: Enable for This Project" to enable';

        items.push({
          label: '📡 MCP Gateway',
          description: gwEnabled ? gwStatus : 'Disabled',
          detail: gwEnabled ? gwDetail : 'Saves ~10 GB RAM by sharing MCP servers across Claude sessions',
        });
      } catch {
        // Gateway services not available
      }

      items.push(
        { kind: vscode.QuickPickItemKind.Separator, label: '' },
      );

      // Add terminal entries
      for (const terminal of terminals) {
        items.push({
          label: `$(terminal) ${terminal.name}`,
          description: terminal.screenSession,
          detail: `Created: ${new Date(terminal.createdAt).toLocaleString()}`,
        });
      }

      // Add actions
      items.push(
        { kind: vscode.QuickPickItemKind.Separator, label: '' },
        { label: '$(refresh) Sync Now', description: 'Sync terminal names' },
        { label: '$(trash) Cleanup Stale', description: 'Remove orphaned sessions' },
        { label: '🧠 Configure Memory', description: 'Configure memory services' },
        { label: '$(output) View Logs', description: 'Open ImmorTerm output' },
        { label: '📡 Open MCP Dashboard', description: 'Open gateway control panel' }
      );

      const selected = await vscode.window.showQuickPick(items, {
        placeHolder: 'ImmorTerm Status',
        title: 'ImmorTerm',
      });

      if (selected?.label === '$(refresh) Sync Now') {
        vscode.commands.executeCommand('immorterm.syncNow');
      } else if (selected?.label === '$(trash) Cleanup Stale') {
        vscode.commands.executeCommand('immorterm.cleanupStale');
      } else if (selected?.label === '🧠 Configure Memory') {
        vscode.commands.executeCommand('immorterm.configureServices');
      } else if (selected?.label === '$(output) View Logs') {
        logger.show();
      } else if (selected?.label === '📡 Open MCP Dashboard') {
        vscode.commands.executeCommand('immorterm.openGatewayDashboard');
      }
    }
  );
  context.subscriptions.push(showStatusCmd);

  // Command: Sync Now
  // T035: Triggers manual sync of terminal names to storage
  const syncNowCmd = vscode.commands.registerCommand(
    'immorterm.syncNow',
    async () => {
      // Sync terminal names from VS Code to storage
      let synced = 0;
      for (const terminal of vscode.window.terminals) {
        const windowId = terminalManager.getWindowIdForTerminal(terminal);
        if (windowId) {
          const updated = await terminalManager.updateTerminalName(
            windowId,
            terminal.name
          );
          if (updated) {
            synced++;
          }
        }
      }

      // Show notification on completion
      notifications.showSyncComplete();
      await statusBar.update();

      logger.info('Synced terminal names:', synced);
    }
  );
  context.subscriptions.push(syncNowCmd);

  // Command: Rename Terminal
  // T036: Rename terminal via VS Code input box (Ctrl+Shift+R)
  const renameTerminalCmd = vscode.commands.registerCommand(
    'immorterm.renameTerminal',
    async () => {
      const terminal = vscode.window.activeTerminal;
      if (!terminal) {
        vscode.window.showWarningMessage('ImmorTerm: No active terminal');
        return;
      }

      const windowId = terminalManager.getWindowIdForTerminal(terminal);
      if (!windowId) {
        vscode.window.showWarningMessage('ImmorTerm: Terminal not tracked by ImmorTerm');
        return;
      }

      const storage = terminalManager.getStorage();
      const projectName = terminalManager.getProjectName();
      const oldVsCodeName = terminal.name; // Capture VS Code tab name before rename
      const result = await renameTerminal(terminal, windowId, storage, projectName);

      if (result.success && result.oldName !== result.newName) {
        // Mark this rename so onDidChangeTerminalState won't revert it
        // VS Code tab name stays as oldVsCodeName (can't update via OSC through screen)
        // Storage/JSON have newName - we don't want handler to "sync" old name back
        markCommandRenamed(windowId, result.newName!, oldVsCodeName);
        vscode.window.showInformationMessage(
          `ImmorTerm: Renamed "${result.oldName}" → "${result.newName}"`
        );
        await statusBar.update();
      } else if (!result.success && result.error !== 'Cancelled') {
        vscode.window.showErrorMessage(`ImmorTerm: ${result.error}`);
      }
    }
  );
  context.subscriptions.push(renameTerminalCmd);

  // Command: Toggle Title Lock (Ctrl+Shift+L)
  const toggleTitleLockCmd = vscode.commands.registerCommand(
    'immorterm.toggleTitleLock',
    async () => {
      const terminal = vscode.window.activeTerminal;
      if (!terminal) {
        vscode.window.showWarningMessage('ImmorTerm: No active terminal');
        return;
      }

      const windowId = terminalManager.getWindowIdForTerminal(terminal);
      if (!windowId) {
        vscode.window.showWarningMessage('ImmorTerm: Terminal not tracked by ImmorTerm');
        return;
      }

      const storage = terminalManager.getStorage();
      const projectName = terminalManager.getProjectName();
      const result = await toggleTitleLock(windowId, storage, projectName, terminal);

      if (result.success) {
        const status = result.locked ? 'locked' : 'unlocked';
        vscode.window.showInformationMessage(
          `ImmorTerm: Title ${status} — "${result.name}"`
        );
        await statusBar.update();
      } else {
        vscode.window.showErrorMessage(`ImmorTerm: ${result.error}`);
      }
    }
  );
  context.subscriptions.push(toggleTitleLockCmd);

  // NOTE: Enable and Disable commands are registered in registerEarlyCommands()
  // so they're available even when activateWorkspace() crashes (e.g. OOM).


  // Command: Apply Theme
  // Shows theme picker and applies the selected theme to screenrc
  const applyThemeCmd = vscode.commands.registerCommand(
    'immorterm.applyTheme',
    async () => {
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      if (!workspaceFolder) {
        vscode.window.showWarningMessage('ImmorTerm: No workspace folder open');
        return;
      }

      const fs = await import('fs/promises');
      const path = await import('path');
      const { getTheme, generateHardstatus, getThemeNames, themeLabels } = await import('./themes');

      const { getProjectScreenrcPath: getScreenrcPath } = await import('./utils/immorterm-config');
      const screenrcPath = getScreenrcPath(workspaceFolder.uri.fsPath);
      const templatePath = path.join(context.extensionPath, 'resources', 'screenrc.template');

      try {
        // Check if per-project screenrc exists
        await fs.access(screenrcPath);
      } catch {
        vscode.window.showWarningMessage('ImmorTerm: No terminal config found. Open a terminal first.');
        return;
      }

      // Get current theme
      const config = vscode.workspace.getConfiguration('immorterm');
      const currentTheme = config.get<string>('statusBarTheme', 'Purple Haze');

      const themeNames = getThemeNames();
      const themeItems = themeNames.map(name => ({
        label: themeLabels[name] || name,
        description: name === currentTheme ? '(current)' : undefined,
        themeName: name,
      }));

      const selectedTheme = await vscode.window.showQuickPick(themeItems, {
        placeHolder: 'Select a theme to apply',
        title: 'ImmorTerm - Apply Theme',
      });

      if (!selectedTheme) {
        return; // User cancelled
      }

      try {
        // Read the EXISTING screenrc (not template) to preserve customizations
        const existingContent = await fs.readFile(screenrcPath, 'utf-8');
        const theme = getTheme(selectedTheme.themeName);

        // Generate the themed hardstatus line
        const themedHardstatus = `hardstatus alwayslastline ${generateHardstatus(theme)}`;

        // Replace ONLY the hardstatus line, preserving everything else
        const themedContent = existingContent.replace(
          /^hardstatus alwayslastline .+$/m,
          themedHardstatus
        );

        // Write back the modified screenrc
        await fs.writeFile(screenrcPath, themedContent, 'utf-8');

        // Save theme to workspace settings
        await config.update('statusBarTheme', selectedTheme.themeName, vscode.ConfigurationTarget.Workspace);

        // Write-through to config.json
        const { getStableProjectId: getThemeProjectId } = await import('./services/memory');
        setConfigTheme(workspaceFolder.uri.fsPath, selectedTheme.themeName, getThemeProjectId(workspaceFolder.uri.fsPath));

        // Auto-apply theme to all open terminals without per-terminal override
        const screenBinary = config.get<string>('screenBinary', 'immorterm');
        const hardstatusLine = generateHardstatus(theme);

        // Get project name for computing screenSession
        const projectName = workspaceFolder.name.toLowerCase().replace(/[^a-z0-9-]/g, '-');

        // Read terminals from registry (source of truth) instead of just workspaceState
        const { getAllTerminalsFromRegistry } = await import('./registry-client');
        const allTerminals = getAllTerminalsFromRegistry();

        logger.info('Auto-applying theme to terminals. Total terminals from JSON:', allTerminals.length);

        let appliedCount = 0;
        let skippedCount = 0;

        const { exec } = await import('child_process');
        const { promisify } = await import('util');
        const execAsync = promisify(exec);

        for (const terminal of allTerminals) {
          // Compute screenSession from projectName and windowId
          const screenSession = `${projectName}-${terminal.windowId}`;

          logger.info('Processing terminal:', terminal.name, 'windowId:', terminal.windowId, 'screenSession:', screenSession, 'theme:', terminal.theme || '(none)');

          if (terminal.theme) {
            // Has per-terminal theme override - skip
            skippedCount++;
            logger.info('Skipping terminal with per-terminal theme:', terminal.name, '->', terminal.theme);
            continue;
          }

          if (!terminal.windowId) {
            logger.info('Skipping terminal without windowId:', terminal.name);
            continue;
          }

          try {
            const command = `${screenBinary} -S "${screenSession}" -X hardstatus alwayslastline ${hardstatusLine}`;
            logger.info('Executing command:', command);
            await execAsync(command);
            appliedCount++;
            logger.info('Applied project theme to terminal:', terminal.name);
          } catch (execErr) {
            // Screen command failed - might be a stale session
            logger.info('Failed to apply theme to terminal:', terminal.name, 'error:', execErr);
          }
        }

        logger.info('Applied theme:', selectedTheme.themeName, 'to', screenrcPath, `(${appliedCount} terminals updated, ${skippedCount} skipped)`);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: Failed to apply theme - ${message}`);
        logger.error('Failed to apply theme:', err);
      }
    }
  );
  context.subscriptions.push(applyThemeCmd);

  // Command: Set Theme for Current Terminal
  // Sets a per-terminal theme that persists on restore
  const setTerminalThemeCmd = vscode.commands.registerCommand(
    'immorterm.setTerminalTheme',
    async () => {
      const activeTerminal = vscode.window.activeTerminal;
      if (!activeTerminal) {
        vscode.window.showWarningMessage('ImmorTerm: No active terminal');
        return;
      }

      const windowId = terminalManager.getWindowIdForTerminal(activeTerminal);
      if (!windowId) {
        vscode.window.showWarningMessage('ImmorTerm: Current terminal is not tracked by ImmorTerm');
        return;
      }

      const storage = terminalManager.getStorage();
      const terminalState = storage.getTerminal(windowId);
      if (!terminalState) {
        vscode.window.showWarningMessage('ImmorTerm: Terminal not found in storage');
        return;
      }

      const { getTheme, generateHardstatus, getThemeNames, themeLabels } = await import('./themes');
      const { exec } = await import('child_process');
      const { promisify } = await import('util');
      const execAsync = promisify(exec);

      // Get current theme (per-terminal if set, otherwise project default)
      const config = vscode.workspace.getConfiguration('immorterm');
      const projectDefault = config.get<string>('statusBarTheme', 'Purple Haze');
      const currentTheme = terminalState.theme || projectDefault;

      const themeNames = getThemeNames();
      const themeItems = themeNames.map(name => ({
        label: themeLabels[name] || name,
        description: name === currentTheme
          ? (terminalState.theme ? '(current - per-terminal)' : '(current - project default)')
          : undefined,
        themeName: name,
      }));

      // Add option to clear per-terminal theme
      const clearOption = {
        label: '$(close) Clear per-terminal theme',
        description: `Use project default (${projectDefault})`,
        themeName: '',
      };
      themeItems.unshift(clearOption);

      const selectedTheme = await vscode.window.showQuickPick(themeItems, {
        placeHolder: `Select theme for "${terminalState.name}"`,
        title: 'ImmorTerm - Set Terminal Theme',
      });

      if (!selectedTheme) {
        return; // User cancelled
      }

      try {
        // Determine the actual theme to apply
        const themeName = selectedTheme.themeName || projectDefault;
        const theme = getTheme(themeName);
        const hardstatusLine = generateHardstatus(theme);

        // Update the terminal state in workspaceState
        if (selectedTheme.themeName) {
          await storage.updateTerminal(windowId, { theme: selectedTheme.themeName });
          logger.info('Set per-terminal theme:', terminalState.name, '->', selectedTheme.themeName);
        } else {
          // Clear per-terminal theme (set to undefined)
          const state = storage.getState();
          const terminal = state.terminals.find(t => t.windowId === windowId);
          if (terminal) {
            delete terminal.theme;
            await storage.setState(state);
          }
          logger.info('Cleared per-terminal theme for:', terminalState.name, '(using project default:', projectDefault, ')');
        }

        // Also update restore-terminals.json
        updateRegistryTheme(windowId, selectedTheme.themeName || undefined);

        // Apply theme to running screen session via screen -X
        const screenBinary = config.get<string>('screenBinary', 'immorterm');
        const screenSession = terminalState.screenSession;

        // Use screen -X to send the hardstatus command to the running session
        const command = `${screenBinary} -S "${screenSession}" -X hardstatus alwayslastline ${hardstatusLine}`;

        try {
          await execAsync(command);
        } catch (execErr) {
          // Screen command failed - theme will still apply on next restore
          logger.warn('Failed to apply theme to running session (will apply on next restore):', execErr);
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: Failed to set theme - ${message}`);
        logger.error('Failed to set terminal theme:', err);
      }
    }
  );
  context.subscriptions.push(setTerminalThemeCmd);

  // Configure Memory Services command
  const configureServicesCmd = vscode.commands.registerCommand(
    'immorterm.configureServices',
    async () => {
      try {
        const { showServicesPicker, isMemoryEnabled, getStableProjectId, installMemoryHooks, areHooksInstalled, validateAndPrompt, removeMemoryHooks } = await import('./services/memory');

        // Show services picker
        const enabledServices = await showServicesPicker();

        if (enabledServices.length === 0) {
          // User cancelled or disabled all services
          // Remove hooks if memory was disabled
          const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
          if (workspaceFolder && !isMemoryEnabled()) {
            removeMemoryHooks(workspaceFolder.uri.fsPath);
            logger.info('Memory hooks removed');
          }
          return;
        }

        // If memory enabled, install hooks
        if (isMemoryEnabled()) {
          const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
          if (workspaceFolder) {
            const prerequisitesMet = await validateAndPrompt();
            if (prerequisitesMet) {
              const projectId = getStableProjectId(workspaceFolder.uri.fsPath);
              installMemoryHooks(workspaceFolder.uri.fsPath, projectId);
              logger.info('Memory hooks installed/updated for project:', projectId);
            }
          }
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: Failed to configure services - ${message}`);
        logger.error('Failed to configure services:', err);
      }
    }
  );


  // Command: Doctor (Diagnostics and Auto-Fix)
  // Runs diagnostics on memory services and offers auto-fix
  const doctorCmd = vscode.commands.registerCommand(
    'immorterm.doctor',
    async () => {
      try {
        const { runDiagnostics, tryAutoFix, isMemoryEnabled } = await import('./services/memory');

        if (!isMemoryEnabled()) {
          vscode.window.showInformationMessage(
            'ImmorTerm: Memory services are disabled. Enable them first via "Configure Memory Services".'
          );
          return;
        }

        // Run diagnostics
        const report = await runDiagnostics();

        // Show diagnostics in output channel
        logger.show();
        logger.info('=== ImmorTerm Doctor ===');
        logger.info(report);

        // Offer auto-fix option
        const selection = await vscode.window.showInformationMessage(
          'ImmorTerm Doctor: Diagnostics complete. Check Output for details.',
          'Try Auto-Fix',
          'Open Web UI',
          'Dismiss'
        );

        if (selection === 'Try Auto-Fix') {
          vscode.window.withProgress(
            {
              location: vscode.ProgressLocation.Notification,
              title: 'ImmorTerm: Running auto-fix...',
              cancellable: false,
            },
            async () => {
              const fixed = await tryAutoFix();
              if (fixed) {
                vscode.window.showInformationMessage(
                  'ImmorTerm: Auto-fix successful! Memory services are now running.',
                  'Open Web UI'
                ).then((sel) => {
                  if (sel === 'Open Web UI') {
                    vscode.env.openExternal(vscode.Uri.parse(`http://localhost:${getMemoryPort()}`));
                  }
                });
              } else {
                vscode.window.showErrorMessage(
                  'ImmorTerm: Auto-fix failed. Please check Docker is running and try again.',
                  'Run Doctor Again'
                ).then((sel) => {
                  if (sel === 'Run Doctor Again') {
                    vscode.commands.executeCommand('immorterm.doctor');
                  }
                });
              }
            }
          );
        } else if (selection === 'Open Web UI') {
          vscode.env.openExternal(vscode.Uri.parse(`http://localhost:${getMemoryPort()}`));
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm Doctor: ${message}`);
        logger.error('Doctor command failed:', err);
      }
    }
  );
  context.subscriptions.push(doctorCmd);
  context.subscriptions.push(configureServicesCmd);

  // Phase A T11 — Digest LLM picker (wizard / menu / CLI all share
  // the same `pickDigestLlm` function under the hood).
  const configureDigestLlmCmd = vscode.commands.registerCommand(
    'immorterm.configureDigestLlm',
    async () => {
      try {
        const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
        if (!workspaceFolder) {
          vscode.window.showWarningMessage(
            'ImmorTerm: Open a workspace folder first — the digest LLM choice is per-project.'
          );
          return;
        }
        const { pickDigestLlm } = await import('./services/memory/digest-llm-picker');
        const { readProjectConfig } = await import('./utils/immorterm-config');
        const existing = readProjectConfig(workspaceFolder.uri.fsPath);
        const initialProvider = existing?.services?.digest?.provider as
          | 'anthropic-cli' | 'anthropic-api' | 'openai-api' | 'gemini-api' | 'ollama' | 'llm-cli'
          | undefined;
        const initialModel = existing?.services?.digest?.model;
        const choice = await pickDigestLlm({
          workspacePath: workspaceFolder.uri.fsPath,
          initialProvider,
          initialModel,
        });
        if (choice) {
          const flag = choice.validated ? 'verified' : 'saved without test';
          vscode.window.showInformationMessage(
            `Digest LLM (${flag}): ${choice.model} via ${choice.provider}`
          );
        }
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: Digest LLM picker failed — ${message}`);
        logger.error('configureDigestLlm failed:', err);
      }
    }
  );
  context.subscriptions.push(configureDigestLlmCmd);

  // License activation — allows users to activate Pro from within VS Code
  const activateLicenseCmd = vscode.commands.registerCommand(
    'immorterm.activateLicense',
    async () => {
      const key = await vscode.window.showInputBox({
        prompt: 'Enter your ImmorTerm Pro license key',
        placeHolder: 'XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX',
        ignoreFocusOut: true,
      });

      if (!key) return;

      try {
        await vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: 'ImmorTerm: Activating license...',
            cancellable: false,
          },
          async () => {
            // Dynamic import to avoid loading license module at startup
            const { activateLicense } = await import('@immorterm/license' as any);
            const { readGlobalConfig, writeGlobalConfig } = await import('@immorterm/config' as any);

            const result = await activateLicense(key);
            if (result.success) {
              const config = readGlobalConfig();
              config.license.key = key;
              config.license.status = 'active';
              config.license.tier = result.license?.tier ?? 'pro';
              config.license.customerEmail = result.license?.email ?? null;
              config.license.expiresAt = result.license?.expiresAt ?? null;
              config.license.instanceId = result.license?.instanceId ?? null;
              config.license.productId = result.license?.productId?.toString() ?? null;
              config.license.lastValidatedAt = new Date().toISOString();
              writeGlobalConfig(config);

              const tierName = config.license.tier === 'memory-pro' ? 'Memory Pro' : 'Pro';
              const configureNow = await vscode.window.showInformationMessage(
                `ImmorTerm ${tierName} activated! (${result.license?.email ?? ''})`,
                'Configure Services',
                'Dismiss'
              );
              if (configureNow === 'Configure Services') {
                vscode.commands.executeCommand('immorterm.configureServices');
              }
            } else {
              vscode.window.showErrorMessage(`ImmorTerm: Activation failed — ${result.error}`);
            }
          }
        );
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`ImmorTerm: License activation error — ${message}`);
        logger.error('License activation failed:', err);
      }
    }
  );
  context.subscriptions.push(activateLicenseCmd);

  // TEST Command: Try to set terminal title via sendText with OSC
  const testTitleCmd = vscode.commands.registerCommand(
    'immorterm.testTitle',
    async () => {
      const terminal = vscode.window.activeTerminal;
      if (!terminal) {
        vscode.window.showErrorMessage('No active terminal');
        return;
      }

      const newName = await vscode.window.showInputBox({
        prompt: 'Enter test title',
        value: 'TestTitle',
      });

      if (!newName) return;

      // Try sending OSC sequence via sendText
      // This sends to terminal INPUT - let's see what happens
      terminal.sendText(`printf '\\033]0;${newName}\\007'`, true);

      vscode.window.showInformationMessage(`Sent OSC for: ${newName}`);
      logger.info(`TEST: Sent OSC via sendText for: ${newName}`);
    }
  );
  context.subscriptions.push(testTitleCmd);

  // Command: Reattach Shelved Terminal
  const reattachCmd = vscode.commands.registerCommand(
    'immorterm.reattachTerminal',
    async () => {
      await reattachTerminal(terminalManager, immortermProvider);
    },
  );
  context.subscriptions.push(reattachCmd);

  // Command: Open MCP Gateway Dashboard
  const openDashboardCmd = vscode.commands.registerCommand(
    'immorterm.openGatewayDashboard',
    async () => {
      const { openGatewayDashboard } = await import('./services/mcp-gateway');
      openGatewayDashboard();
    },
  );
  context.subscriptions.push(openDashboardCmd);

  logger.info('Registered 14 commands');
}

/**
 * Subscribes to terminal events for tracking and lifecycle management
 */
function subscribeToTerminalEvents(
  context: vscode.ExtensionContext,
  terminalManager: TerminalManager,
  statusBar: StatusBar
): void {
  // Diagnostic logging helper — writes to .immorterm/diagnostic.log
  const diagLog = (msg: string) => {
    try {
      const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
      if (ws) {
        const logPath = require('path').join(ws, '.immorterm', 'diagnostic.log');
        require('fs').appendFileSync(logPath, `[${new Date().toISOString()}] ${msg}\n`);
      }
    } catch { /* ignore */ }
  };

  // Track when terminals are opened
  const onDidOpenTerminal = vscode.window.onDidOpenTerminal((terminal) => {
    logger.info('Terminal opened:', terminal.name);
    const opts = terminal.creationOptions as vscode.TerminalOptions | undefined;
    diagLog(`OPEN: name="${terminal.name}" shell="${opts?.shellPath ?? '?'}" envWID=${opts?.env?.IMMORTERM_WINDOW_ID ?? 'NONE'} hidden=${(opts as any)?.hideFromUser ?? false} alreadyTracked=${!!terminalManager.getWindowIdForTerminal(terminal)} totalTerminals=${vscode.window.terminals.length}`);

    // Check if this terminal is already tracked (restored terminal)
    if (terminalManager.getWindowIdForTerminal(terminal)) {
      logger.debug('Terminal already tracked:', terminal.name);
      return;
    }

    // Check for ImmorTerm env var - most reliable method
    // The profile provider sets IMMORTERM_WINDOW_ID in the terminal's env
    const envWindowId = opts?.env?.IMMORTERM_WINDOW_ID;
    if (envWindowId) {
      terminalManager.trackTerminal(terminal, envWindowId);
      // Re-enabled: ChangeAKA no longer calls WindowChanged(), safe to use
      addTitleFileWatcher(envWindowId, terminalManager);
      logger.info('Tracked terminal by env var:', envWindowId);
      return;
    }

    // Fallback: Try to match this terminal to a registered entry by name
    // This handles restored terminals that don't have the env var
    const storage = terminalManager.getStorage();
    const terminalState = storage.getTerminalByName(terminal.name);
    logger.debug('Looking for terminal by name:', terminal.name, '-> found:', !!terminalState);

    if (terminalState) {
      // Found matching terminal in storage - track it!
      terminalManager.trackTerminal(terminal, terminalState.windowId);
      // Re-enabled: ChangeAKA no longer calls WindowChanged(), safe to use
      addTitleFileWatcher(terminalState.windowId, terminalManager);
      logger.info('Tracked new terminal by name:', terminal.name, '->', terminalState.windowId);
    } else {
      // Terminal not in storage yet - might be reconciled later via pending file
      // Use a small delay to check again after pending file is processed
      setTimeout(() => {
        if (!terminalManager.getWindowIdForTerminal(terminal)) {
          const retryState = storage.getTerminalByName(terminal.name);
          if (retryState) {
            terminalManager.trackTerminal(terminal, retryState.windowId);
            // Re-enabled: ChangeAKA no longer calls WindowChanged(), safe to use
            addTitleFileWatcher(retryState.windowId, terminalManager);
            logger.info('Tracked terminal after delay:', terminal.name, '->', retryState.windowId);
          }
        }
      }, 500);
    }
  });
  context.subscriptions.push(onDidOpenTerminal);
  disposables.push(onDidOpenTerminal);

  // Track when terminals are closed
  const onDidCloseTerminal = vscode.window.onDidCloseTerminal((terminal) => {
    try {
    diagLog(`CLOSE_RAW: name="${terminal.name}" pid=${terminal.processId ?? '?'}`);
    logger.debug('Terminal closed:', terminal.name);

    const windowId = terminalManager.getWindowIdForTerminal(terminal);
    const trackedMapSize = terminalManager.getTrackedCount();
    const storageCount = terminalManager.getTerminalCount();
    const trackedIds = terminalManager.getTrackedWindowIds();
    diagLog(`CLOSE: name="${terminal.name}" windowId=${windowId ?? 'NULL'} mapSize=${trackedMapSize} storageCount=${storageCount} restorationInProgress=${restorationInProgress} trackedIds=[${trackedIds.join(',')}] totalVSCodeTerminals=${vscode.window.terminals.length}`);
    if (windowId) {
      // Untrack from memory
      terminalManager.untrackTerminal(terminal);

      // Clear any pending rename tracking for this terminal
      commandRenames.delete(windowId);

      // Clear restored terminal tracking (if within grace period)
      restoredTerminals.delete(windowId);

      // Remove title file watcher
      removeTitleFileWatcher(windowId, terminalManager);

      // Schedule cleanup with grace period (prevents accidental cleanup during VS Code reload)
      if (initResult?.logsDir) {
        const scheduled = terminalManager.scheduleCleanup(windowId, initResult.logsDir);
        logger.info(`Terminal closed: ${windowId} - cleanup ${scheduled ? 'scheduled' : 'already pending'}`);
      } else {
        logger.warn(`Terminal closed: ${windowId} - cleanup NOT scheduled (logsDir missing)`);
      }

      // Update status bar
      statusBar.update().catch((err) => {
        logger.error('Failed to update status bar:', err);
      });

      // Memory digestion is owned by the Rust singleton daemon. Its
      // BurstQuiet debouncer fires within ~2 min of conversation
      // end, so the prior "immediate digest on terminal close" trigger
      // is redundant and was removed.
    }
    } catch (err) {
      diagLog(`CLOSE_ERROR: ${err instanceof Error ? err.message : String(err)}`);
      logger.error('Error in onDidCloseTerminal handler:', err);
    }
  });
  context.subscriptions.push(onDidCloseTerminal);
  disposables.push(onDidCloseTerminal);

  // Heartbeat + title sync are folded into the 30s Claude sync loop below.
  // No separate timers or per-session watchers needed.

  // Track terminal state changes (may include name changes)
  const onDidChangeTerminalState = vscode.window.onDidChangeTerminalState(
    async (terminal) => {
      logger.debug('Terminal state changed:', terminal.name);

      const windowId = terminalManager.getWindowIdForTerminal(terminal);
      logger.debug('onDidChangeTerminalState - windowId lookup:', windowId || 'NOT FOUND');

      if (windowId) {
        // Check if name changed and update storage
        const storedTerminal = terminalManager.getTerminalByWindowId(windowId);
        logger.debug('onDidChangeTerminalState - stored name:', storedTerminal?.name || 'NOT FOUND', '| terminal name:', terminal.name);

        if (storedTerminal && storedTerminal.name !== terminal.name) {
          // Check if we should skip this sync (command-initiated rename where VS Code hasn't updated)
          if (shouldSkipVsCodeSync(windowId, terminal.name, storedTerminal.name)) {
            return;
          }
          // VS Code name actually changed (user manual rename) - sync all
          await syncUserRename(windowId, terminal.name, terminalManager);
        } else {
          logger.debug('onDidChangeTerminalState - name unchanged or terminal not in storage');
        }
      } else {
        logger.debug('onDidChangeTerminalState - terminal not tracked by ImmorTerm');
      }
    }
  );
  context.subscriptions.push(onDidChangeTerminalState);
  disposables.push(onDidChangeTerminalState);

  // Track active terminal changes - also sync names here since onDidChangeTerminalState
  // may not fire reliably for name changes
  const onDidChangeActiveTerminal = vscode.window.onDidChangeActiveTerminal(
    async (terminal) => {
      if (terminal) {
        logger.debug('Active terminal changed:', terminal.name);

        // Check if this terminal's name changed (user manual rename via VS Code UI)
        const windowId = terminalManager.getWindowIdForTerminal(terminal);
        if (windowId) {
          // Tell SessionManager which terminal is active (for stats toggle gating)
          const sm = getSessionManager();
          sm?.setActiveWindowId(windowId);

          // Persist last-focused terminal per type (for focus restoration on restart)
          const sessionType = sm?.getSessionType(windowId);
          if (sessionType) {
            setActiveTerminal(sessionType, windowId);
          }

          const storedTerminal = terminalManager.getTerminalByWindowId(windowId);
          if (storedTerminal && storedTerminal.name !== terminal.name) {
            // Check if we should skip this sync (command-initiated rename)
            if (shouldSkipVsCodeSync(windowId, terminal.name, storedTerminal.name)) {
              return;
            }
            // VS Code name actually changed (user manual rename) - sync all
            await syncUserRename(windowId, terminal.name, terminalManager);
          }

          // PERF: Removed tab-switch redisplay — it spawned a child process on every
          // tab switch (screen -X redisplay), contributing ~20+ process spawns/min.
          // The original xterm.js resize race is no longer observed in current VS Code.
        }
      } else {
        // No terminal active — clear the active window ID
        getSessionManager()?.setActiveWindowId(null);
      }
    }
  );
  context.subscriptions.push(onDidChangeActiveTerminal);
  disposables.push(onDidChangeActiveTerminal);

  logger.info('Subscribed to terminal events');
}

/**
 * Sets up a file watcher for the pending directory
 *
 * When screen-auto creates a new terminal, it writes a pending file with:
 *   {windowId} {displayName}
 *
 * This watcher:
 * 1. Watches .immorterm/terminals/pending/ for new files
 * 2. Reads the file content to get windowId and displayName
 * 3. Calls reconcileTerminal() to register the terminal
 * 4. Deletes the pending file after successful processing
 */
function setupPendingFileWatcher(
  context: vscode.ExtensionContext,
  terminalManager: TerminalManager,
  statusBar: StatusBar
): vscode.FileSystemWatcher | undefined {
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  if (!workspaceFolder) {
    logger.warn('No workspace folder, skipping pending file watcher');
    return undefined;
  }

  const pendingDir = getPendingDirFromConfig(workspaceFolder.uri.fsPath);
  const pendingPattern = new vscode.RelativePattern(pendingDir, '*');

  // Ensure pending directory exists
  fs.mkdir(pendingDir, { recursive: true }).catch((err) => {
    logger.debug('Could not create pending directory:', err);
  });

  const watcher = vscode.workspace.createFileSystemWatcher(pendingPattern);

  // Processing queue to serialize pending file handling (prevents race conditions)
  const pendingQueue: vscode.Uri[] = [];
  let isProcessing = false;

  // Process the next item in the queue
  const processQueue = async (): Promise<void> => {
    if (isProcessing || pendingQueue.length === 0) {
      return;
    }
    isProcessing = true;

    while (pendingQueue.length > 0) {
      const uri = pendingQueue.shift()!;
      await processPendingFile(uri);
    }

    isProcessing = false;
  };

  // Add to queue and trigger processing
  const enqueueFile = (uri: vscode.Uri): void => {
    pendingQueue.push(uri);
    processQueue();
  };

  // Process a pending file
  const processPendingFile = async (uri: vscode.Uri): Promise<void> => {
    const filePath = uri.fsPath;
    const fileName = path.basename(filePath);

    try {
      // Read the pending file content
      const content = await fs.readFile(filePath, 'utf-8');
      const trimmed = content.trim();

      if (!trimmed) {
        logger.debug(`Empty pending file, deleting: ${fileName}`);
        await fs.unlink(filePath).catch(() => {});
        return;
      }

      // Parse format: "{windowId} {displayName}"
      const spaceIndex = trimmed.indexOf(' ');
      if (spaceIndex === -1) {
        logger.warn(`Invalid pending file format (no space): ${fileName}`);
        await fs.unlink(filePath).catch(() => {});
        return;
      }

      const windowId = trimmed.substring(0, spaceIndex);
      const displayName = trimmed.substring(spaceIndex + 1);

      logger.info(`Processing pending terminal: ${windowId} -> ${displayName}`);

      // Reconcile the terminal (register in storage)
      const storage = terminalManager.getStorage();
      const result = await reconcileTerminal(windowId, displayName, storage);

      if (result.added) {
        logger.info(`Registered new terminal: ${windowId} (${displayName})`);
        // Note: Terminal tracking now happens in onDidOpenTerminal via env var check
        // This ensures deterministic matching instead of racy "first untracked" scanning

        // Update status bar to reflect new terminal count
        await statusBar.update();
      } else {
        logger.debug(`Terminal already registered: ${windowId}`);
      }

      // Delete the pending file after successful processing
      await fs.unlink(filePath).catch((err) => {
        logger.warn(`Failed to delete pending file ${fileName}:`, err);
      });

    } catch (err) {
      logger.error(`Failed to process pending file ${fileName}:`, err);
      // Still try to delete the file to avoid infinite retries
      await fs.unlink(filePath).catch(() => {});
    }
  };

  // Watch for new files - enqueue for serialized processing
  watcher.onDidCreate((uri) => {
    logger.debug('Pending file created:', uri.fsPath);
    // Small delay to ensure file is fully written, then enqueue
    setTimeout(() => enqueueFile(uri), 100);
  });

  // Also process any existing pending files on startup
  (async () => {
    try {
      const files = await fs.readdir(pendingDir);
      for (const file of files) {
        const filePath = path.join(pendingDir, file);
        const uri = vscode.Uri.file(filePath);
        await processPendingFile(uri);
      }
      if (files.length > 0) {
        logger.info(`Processed ${files.length} pending file(s) on startup`);
      }
    } catch (err) {
      // Directory may not exist yet, that's fine
      if ((err as NodeJS.ErrnoException).code !== 'ENOENT') {
        logger.debug('Could not read pending directory:', err);
      }
    }
  })();

  context.subscriptions.push(watcher);
  disposables.push(watcher);

  logger.info('Pending file watcher initialized');
  return watcher;
}

/**
 * Extension activation entry point
 * Called when VS Code starts up (onStartupFinished)
 */
export async function activate(
  context: vscode.ExtensionContext
): Promise<void> {
  logger.info('ImmorTerm extension activating...');

  // Spawn the hub sidecar early — webview HTTP fetches (/api/v1/config,
  // /api/v1/digest/test, etc.) all need a hub on localhost:1440. The
  // standalone Tauri app does this via src-tauri/hub_sidecar.rs; VS Code
  // mirrors it here so both surfaces share the same modal HTTP layer
  // without VS Code-specific postMessage detours. Awaited so popup
  // modals opened immediately after activation hit a live hub.
  try {
    const { ensureHubRunning } = await import('./hub-sidecar');
    await ensureHubRunning();
  } catch (e) {
    logger.warn('hub-sidecar startup failed (modals will fall back to error UI):', e);
  }

  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];

  // RACE CONDITION FIX: Register the terminal profile provider IMMEDIATELY,
  // before any async work. VS Code may try to open the default terminal
  // (set to "ImmorTerm") before activate() finishes its async initialization.
  // The provider awaits initReady internally, so the terminal creation is
  // deferred until everything is ready — no more falling back to zsh.
  let resolveInit: (result: InitializationResult | null) => void;
  const initReady = new Promise<InitializationResult | null>((resolve) => {
    resolveInit = resolve;
  });

  if (workspaceFolder) {
    const terminalsDir = getTerminalsDir(workspaceFolder);
    const projectName = getProjectName(workspaceFolder);
    const pendingDir = getPendingDirFromConfig(workspaceFolder.uri.fsPath);

    const terminalProfileProvider = new ImmorTermProfileProvider(
      getScriptsDir(),
      terminalsDir,
      projectName,
      pendingDir,
      initReady
    );
    context.subscriptions.push(
      vscode.window.registerTerminalProfileProvider('immorterm.screen', terminalProfileProvider)
    );
    logger.info('Terminal profile provider registered (before async init)');
  }

  // Register ImmorTerm Rust GPU terminal — WebviewView in the bottom panel.
  // This has zero dependency on Screen/C binary and must always be available.
  {
    const wsFolder = vscode.workspace.workspaceFolders?.[0];
    const projName = wsFolder?.name || 'immorterm';
    const projPath = wsFolder?.uri.fsPath || '';
    immortermProvider = new ImmorTermViewProvider(context, projName, projPath);

    // Pre-populate sessions from registry BEFORE the view resolves.
    // This MUST happen before registerWebviewViewProvider because VS Code
    // immediately calls resolveWebviewView() → webview sends 'loaded' →
    // handler iterates this.sessions. If we wait until after the slow
    // activateWorkspace() call, the Map is empty when 'loaded' fires.
    // Only auto-restore when terminalMode includes 'ai' AND user has Pro license.
    // Check VS Code settings first, then fall back to project config.json.
    let terminalMode = getTerminalMode();
    if (terminalMode === 'regular' && projPath) {
      const configMode = getConfigTerminalMode(projPath);
      if (configMode === 'ai' || configMode === 'both') {
        terminalMode = configMode;
      }
    }
    const aiModeRequested = terminalMode === 'ai' || terminalMode === 'both';
    // Full Pro only — the memory-only "memory-pro" SKU does not unlock the AI terminal.
    logger.info(`AI restore gate: terminalMode=${terminalMode}, aiModeRequested=${aiModeRequested}, isFullProTier=${isFullProTier()}`);
    if (aiModeRequested && isFullProTier()) {
      // Backfill BEFORE restore — restore reads registry.json and filters by
      // owner_project_id / owner_project_dir. If backfill runs after, the
      // first reload sees legacy entries (no fields) and the worktree-orphan
      // rescue silently fails. Idempotent — no-ops after first activate.
      try {
        const backfilled = backfillOwnerProjectFields();
        if (backfilled > 0) logger.info(`Backfilled owner_project fields on ${backfilled} legacy registry entries (pre-restore)`);
      } catch (err) {
        logger.warn(`backfillOwnerProjectFields (pre-restore) failed: ${err}`);
      }
      immortermProvider.restoreSessions().then(() => {
        logger.info(`AI restore complete: ${immortermProvider!.sessionCount} sessions restored`);
        // If sessions were restored, reveal the IMMORTERM panel to trigger
        // resolveWebviewView(). Without this, if the panel tab is hidden
        // (e.g., Terminal/Output tab active), the view never resolves and
        // sessions appear blank — sendSessionToWebview() silently returns.
        if (immortermProvider!.sessionCount > 0) {
          vscode.commands.executeCommand(`${IMMORTERM_VIEW_ID}.focus`);
        }
      }).catch(err => {
        logger.warn('ImmorTerm Rust session restore failed:', err);
      });
    } else if (aiModeRequested && !isFullProTier()) {
      logger.info('AI terminal mode requested but Pro license required — skipping AI restore');
    } else {
      logger.info(`AI restore SKIPPED: aiMode=${aiModeRequested}, proTier=${isFullProTier()}`);
    }

    const viewProviderDisposable = vscode.window.registerWebviewViewProvider(
      IMMORTERM_VIEW_ID,
      immortermProvider,
      { webviewOptions: { retainContextWhenHidden: true } },
    );
    context.subscriptions.push(viewProviderDisposable);

    const gpuTerminalCmd = vscode.commands.registerCommand(
      'immorterm.newImmortermTerminal',
      async () => {
        try {
          await immortermProvider!.createSession();
        } catch (err) {
          logger.error('ImmorTerm Rust command failed:', err);
          vscode.window.showErrorMessage(`ImmorTerm Error: ${err}`);
        }
      }
    );
    context.subscriptions.push(gpuTerminalCmd);

    // Cmd+Shift+R / Ctrl+Shift+R: forward to the webview so it can send the
    // WS `rerender_backlog` request. Bound in package.json with
    // `when: focusedView == 'immorterm.terminalView'` so it overrides the
    // default `immorterm.renameTerminal` binding only inside our view.
    const rerenderBacklogCmd = vscode.commands.registerCommand(
      'immorterm.rerenderBacklog',
      () => immortermProvider?.postMessageToWebview({ type: 'rerender-backlog' })
    );
    context.subscriptions.push(rerenderBacklogCmd);

    const togglePomodoroCmd = vscode.commands.registerCommand(
      'immorterm.togglePomodoro',
      () => {
        immortermProvider?.postMessageToWebview({ type: 'toggle-pomodoro' });
      },
    );
    context.subscriptions.push(togglePomodoroCmd);

    const newTaskCmd = vscode.commands.registerCommand(
      'immorterm.newTask',
      () => {
        immortermProvider?.postMessageToWebview({ type: 'open-task-modal' });
      },
    );
    context.subscriptions.push(newTaskCmd);

    // Right-click → Send to ImmorTerm. Bridges the gap that VS Code's
    // WebviewView API doesn't expose drop hooks for Explorer drags
    // (terminals do because they're built-in, third-party views don't).
    // Multi-select in Explorer hands us an array of URIs in the second
    // arg; single-select uses the first. Forward all paths to the
    // active webview which inserts them as `@path` (Claude active) or
    // bare path.
    const sendFileCmd = vscode.commands.registerCommand(
      'immorterm.sendFileToActiveSession',
      (single?: vscode.Uri, multi?: vscode.Uri[]) => {
        const uris = (multi && multi.length > 0)
          ? multi
          : (single ? [single] : []);
        const fsPaths = uris
          .filter((u) => u && u.scheme === 'file')
          .map((u) => u.fsPath);
        if (fsPaths.length === 0) {
          vscode.window.showWarningMessage('ImmorTerm: no files selected');
          return;
        }
        immortermProvider?.postMessageToWebview({
          type: 'insert-file-paths',
          paths: fsPaths,
        });
        // Reveal the panel so the user sees the result, especially if
        // they triggered the command from a hidden folder context.
        vscode.commands.executeCommand('immorterm.terminalView.focus');
      },
    );
    context.subscriptions.push(sendFileCmd);
  }
  logger.info('ImmorTerm Rust terminal view registered');

  // DISABLED: teams feature temporarily commented out
  // {
  //   teamViewProvider = new TeamViewProvider(context);
  //   const teamViewDisposable = vscode.window.registerWebviewViewProvider(
  //     TEAM_VIEW_ID,
  //     teamViewProvider,
  //     { webviewOptions: { retainContextWhenHidden: false } },
  //   );
  //   context.subscriptions.push(teamViewDisposable);
  //
  //   const openTeamViewCmd = vscode.commands.registerCommand(
  //     'immorterm.openTeamView',
  //     () => {
  //       vscode.commands.executeCommand('immorterm.teamView.focus');
  //     },
  //   );
  //   context.subscriptions.push(openTeamViewCmd);
  // }
  // logger.info('ImmorTerm Team View registered');

  // Crash-proof diagnostic log (same as activation.ts)
  const wsPath = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath || '';
  const diagLog = (step: string) => {
    try {
      const rss = Math.round(process.memoryUsage().rss / 1024 / 1024);
      fsSync.appendFileSync(
        path.join(wsPath, '.immorterm', 'diagnostic.log'),
        `[${new Date().toISOString()}] EXT_STEP: ${step} (RSS=${rss}MB)\n`
      );
    } catch { /* ignore */ }
  };
  // Register enable/disable commands BEFORE activateWorkspace — these must always
  // be available even if the Extension Host OOMs during activation. Without this,
  // users can't disable ImmorTerm (to stop the OOM crash loop) or re-enable it.
  registerEarlyCommands(context);
  diagLog('earlyCommands registered');

  diagLog('activateWorkspace start');

  // Now do the slow async initialization (detectScreen, extractResources, etc.)
  // GUARD: If activateWorkspace crashes (e.g. OOM at high RSS), we must still resolve
  // initReady so the profile provider doesn't hang forever on Ctrl+Shift+`.
  try {
    initResult = await activateWorkspace(context);
    diagLog('activateWorkspace returned');
  } catch (err) {
    diagLog(`activateWorkspace CRASHED: ${err}`);
    logger.error('activateWorkspace threw — resolving initReady with null:', err);
    resolveInit!(null);
    return;
  }

  // Block profile provider from auto-creating terminals during restoration.
  // Will be cleared after restoreTerminalsWithDelay() completes.
  restorationInProgress = true;

  // Safety timeout: if restoration hangs or an error path forgets to clear the flag,
  // force-clear after 10s so Ctrl+Shift+` always works for the user.
  setTimeout(() => {
    if (restorationInProgress) {
      logger.warn('Safety timeout: clearing restorationInProgress after 10s');
      restorationInProgress = false;
    }
  }, 10_000);

  // Signal the profile provider that initialization is done
  resolveInit!(initResult);

  if (!initResult) {
    logger.warn('ImmorTerm initialization failed or no workspace');
    restorationInProgress = false;
    return;
  }

  const { terminalManager, statusBar, screenAvailable, terminalsDir, logsDir } = initResult;

  // Register all commands — needed even when disabled (so enableForProject works)
  diagLog('registerCommands start');
  registerCommands(context, terminalManager, statusBar, logsDir);
  diagLog('registerCommands done');

  // Check if ImmorTerm is enabled for this project.
  // Skip all file-system-touching operations (watchers, syncs, pending dirs) when not enabled.
  const enabledConfig = vscode.workspace.getConfiguration('immorterm');
  const isEnabled = enabledConfig.inspect<boolean>('enabled')?.workspaceValue === true;

  if (isEnabled) {
    // Initialize registry client (unified terminal state via ~/.immorterm/registry.json)
    logger.info('Workspace folder for registry init:', workspaceFolder?.uri.fsPath || 'NONE');
    if (workspaceFolder) {
      diagLog('initRegistryClient start');
      initRegistryClient(workspaceFolder.uri.fsPath, (msg) => logger.info(msg));
      diagLog('initRegistryClient done');
      logger.info('Initialized registry client for project:', workspaceFolder.uri.fsPath);

      // One-shot: move any legacy shelved entries out of registry.json into
      // registry-shelved.json. Registry.json should only contain sessions the
      // user has NOT deliberately closed. Idempotent after first run.
      try {
        const migrated = migrateShelvedOutOfRegistry();
        if (migrated > 0) logger.info(`Migrated ${migrated} shelved entries to registry-shelved.json`);
      } catch (err) {
        logger.warn(`migrateShelvedOutOfRegistry failed: ${err}`);
      }

      // Sweep stub-zombie entries (pid=0, empty name, >7d old) left behind by
      // reconcileTerminal() for regular VS Code terminals whose stub never got
      // updated with real daemon metadata. Accumulate forever without this.
      try {
        const swept = sweepStubZombies();
        if (swept > 0) logger.info(`Swept ${swept} stub-zombie registry entries`);
      } catch (err) {
        logger.warn(`sweepStubZombies failed: ${err}`);
      }

      // Sweep orphan shelved entries whose backing dir was pruned by the
      // log-cleanup timer. Without this, the reattach UI lists sessions the
      // user can't actually restore — they spawn a fresh empty daemon.
      try {
        const orphans = sweepOrphanShelvedEntries();
        if (orphans > 0) logger.info(`Swept ${orphans} orphan shelved entries (dirs missing on disk)`);
      } catch (err) {
        logger.warn(`sweepOrphanShelvedEntries failed: ${err}`);
      }

      // (backfillOwnerProjectFields is now called earlier, before
      // restoreSessions() — see the pre-restore block above.)

      // Initialize Claude sync if enabled
      if (shouldClaudeAutoResume()) {
        const projectName = workspaceFolder.name;
        const screenBinary = vscode.workspace.getConfiguration('immorterm').get<string>('screenBinary', 'immorterm');
        diagLog('initClaudeSync start');
        initClaudeSync(projectName, workspaceFolder.uri.fsPath, (msg) => logger.info(msg), screenBinary);
        diagLog('initClaudeSync done');
        logger.info('Claude sync initialized for project:', projectName);

        // Install statusline script for Claude Code API stats (project-scoped, idempotent)
        const extensionResourcesPath = path.join(context.extensionPath, 'resources');
        installStatuslineScript(extensionResourcesPath, workspaceFolder.uri.fsPath);
        logger.info('Claude statusline script installed to project .claude/');

        // Callback to refresh status bar after memory health changes
        const onMemoryStateChange = () => {
          statusBar.update().catch(err => logger.error('StatusBar update error:', err));
        };

        // Consolidated sync loop: Claude state + title files + heartbeat
        // Replaces 3 separate timer systems (N stale-check timers, N title watchers, 1 heartbeat)
        const consolidatedSync = () => {
          // Heartbeat (diagnostic log)
          diagLog(`HEARTBEAT: trackedMap=${terminalManager.getTrackedCount()} storage=${terminalManager.getTerminalCount()} vsTerminals=${vscode.window.terminals.length} restorationInProgress=${restorationInProgress}`);
          // Claude state sync (polls context files + registry updates + lifecycle checks)
          syncClaudeSessions(onMemoryStateChange);
          // Batch title file reads (replaces N per-session FileSystemWatchers)
          batchSyncTitleFiles(terminalManager).catch(err =>
            logger.debug('Title batch sync error:', err)
          );
        };

        // Run initial sync
        consolidatedSync();

        // Schedule periodic consolidated sync
        const syncInterval = getClaudeSyncInterval();
        claudeSyncTimer = setInterval(consolidatedSync, syncInterval);
        logger.info('Consolidated sync scheduled every', syncInterval / 1000, 'seconds (Claude state + titles + heartbeat)');

      } else {
        logger.info('Claude auto-resume disabled by settings');
      }

      // Start background auto-update checker (separate long-interval timer)
      try {
        const { startUpdateChecker } = await import('./services/update-checker');
        startUpdateChecker((msg: string) => logger.info(msg));
        logger.info('Auto-update checker started');
      } catch (err) {
        logger.warn('Failed to start auto-update checker:', err);
      }
    }

    // Subscribe to terminal events
    subscribeToTerminalEvents(context, terminalManager, statusBar);

    // Set up pending file watcher for new terminal registration
    setupPendingFileWatcher(context, terminalManager, statusBar);

    // Schedule periodic cleanup tasks
    schedulePeriodicCleanup(terminalManager.getStorage(), logsDir);

    // Start shelved session reaper (checks TTL every 60s)
    startShelvedReaper();
  } else {
    logger.info('ImmorTerm not enabled for this project — skipping watchers and file setup');
  }

  // Migrate default profile settings from legacy "screen" to "immorterm.screen"
  await migrateDefaultProfileSettings();

  // Title sync is now event-driven via onDidWriteTerminalData
  // (no more polling - C code echoes OSC 777;immorterm;title notifications)

  // Log activation summary
  logger.info('ImmorTerm activated', {
    screenAvailable,
    terminalsDir,
    terminalCount: terminalManager.getTerminalCount(),
    workspaceFolder: vscode.workspace.workspaceFolders?.[0]?.name,
  });

  // Check for conflicting "Restore Terminals" extension
  // This extension reads restore-terminals.json and creates duplicate terminals
  const restoreTerminalsExt = vscode.extensions.getExtension('ethanjreesor.restore-terminals');
  if (restoreTerminalsExt) {
    logger.warn('Conflicting extension detected: "Restore Terminals" (ethanjreesor.restore-terminals)');
    logger.warn('This will cause duplicate terminals. Please disable "Restore Terminals" extension.');

    vscode.window.showWarningMessage(
      'ImmorTerm: The "Restore Terminals" extension is installed. This causes duplicate terminals on reload. Please disable "Restore Terminals" - ImmorTerm handles terminal persistence natively.',
      'Open Extensions',
      'Dismiss'
    ).then((choice) => {
      if (choice === 'Open Extensions') {
        vscode.commands.executeCommand('workbench.extensions.action.showInstalledExtensions');
      }
    });
  }

  // Sync terminal names from registry to WorkspaceStorage
  // This ensures names and themes edited in registry or from external sources are respected
  // Only run when enabled — registry client needs projectPath set to filter correctly
  if (isEnabled) {
    const jsonTerminals = getAllTerminalsFromRegistry();
    for (const jsonTerm of jsonTerminals) {
      const storedTerm = terminalManager.getStorage().getTerminal(jsonTerm.windowId);
      if (storedTerm) {
        const updates: { name?: string; theme?: string } = {};

        if (storedTerm.name !== jsonTerm.name) {
          logger.info(`Syncing name from JSON: "${storedTerm.name}" -> "${jsonTerm.name}" for ${jsonTerm.windowId}`);
          updates.name = jsonTerm.name;
        }

        if (storedTerm.theme !== jsonTerm.theme) {
          logger.info(`Syncing theme from JSON: "${storedTerm.theme || 'none'}" -> "${jsonTerm.theme || 'none'}" for ${jsonTerm.windowId}`);
          updates.theme = jsonTerm.theme;
        }

        if (Object.keys(updates).length > 0) {
          await terminalManager.getStorage().updateTerminal(jsonTerm.windowId, updates);
        }
      }
    }
  }

  // Restore terminals on startup (if enabled, Screen is available, and mode includes 'regular').
  // When mode is 'ai' but user lacks Pro, fall back to regular restoration.
  // IMPORTANT: Must check isEnabled — when disabled, registry client has no projectPath
  // and getAllTerminalsFromRegistry() would return ALL projects' terminals.
  let restoreMode = getTerminalMode();
  if (restoreMode === 'regular' && workspaceFolder) {
    const configMode = getConfigTerminalMode(workspaceFolder.uri.fsPath);
    if (configMode === 'ai' || configMode === 'both') {
      restoreMode = configMode;
    }
  }
  const skipRegular = restoreMode === 'ai' && isFullProTier();
  diagLog(`restoration check: enabled=${isEnabled} screen=${screenAvailable} restoreOnStartup=${isRestoreOnStartupEnabled()} mode=${restoreMode} skipRegular=${skipRegular}`);
  if (isEnabled && screenAvailable && isRestoreOnStartupEnabled() && !skipRegular) {
    diagLog('restoreTerminalsWithDelay starting');
    // Use delay to let VS Code fully initialize before creating terminals
    restoreTerminalsWithDelay(
      terminalManager,
      { scriptsPath: getScriptsDir(), terminalsDir },
      100 // Brief startup delay (reduced from 500ms - VS Code is ready by onStartupFinished)
    )
      .then((result) => {
        restorationInProgress = false;

        if (result.restored > 0) {
          logger.info(`Restored ${result.restored} terminal(s) on startup`);
        }
        if (result.failed > 0) {
          logger.warn(`Failed to restore ${result.failed} terminal(s)`);
        }

        // Track restored terminals for name sync
        for (const terminal of vscode.window.terminals) {
          trackTerminalName(terminal);
        }

        // Update status bar after restoration
        statusBar.update();
      })
      .catch((err) => {
        restorationInProgress = false;
        logger.error('Terminal restoration failed:', err);
      });
  } else {
    restorationInProgress = false;
    if (!isEnabled) {
      logger.info('Skipping terminal restoration - ImmorTerm not enabled for this project');
    } else if (!screenAvailable) {
      logger.info('Skipping terminal restoration - Screen not available');
    } else {
      logger.info('Terminal restoration disabled by settings');
    }
  }

  // Note: ImmorTerm Rust session restore already happened above (before
  // registerWebviewViewProvider) to avoid race with webview 'loaded' message.
}

/**
 * Extension deactivation entry point
 * Called when VS Code shuts down or extension is disabled
 */
export async function deactivate(): Promise<void> {
  logger.info('ImmorTerm extension deactivating...');

  // Stop the hub sidecar if we spawned one — don\u2019t leak processes
  // across VS Code reloads. Reusing-an-existing-hub case is a no-op.
  try {
    const { stopHubSidecar } = require('./hub-sidecar');
    stopHubSidecar();
  } catch { /* ignore */ }

  // Cancel scheduled cleanup timers
  cancelScheduledCleanup();

  // Stop shelved session reaper
  stopShelvedReaper();

  // Stop auto-update checker
  try {
    const { stopUpdateChecker } = require('./services/update-checker');
    stopUpdateChecker();
  } catch {}

  // Dispose terminal event subscriptions
  for (const disposable of disposables) {
    disposable.dispose();
  }
  disposables = [];

  // Dispose ImmorTerm Rust view provider
  if (immortermProvider) {
    immortermProvider.dispose();
    immortermProvider = null;
  }

  // DISABLED: teams feature temporarily commented out
  // if (teamViewProvider) {
  //   teamViewProvider.dispose();
  //   teamViewProvider = null;
  // }

  // Dispose Claude sync (SessionManager watchers + WebSocket connections)
  disposeClaudeSync();

  // Flush any pending registry writes before exit
  flushRegistryWrites();

  // Close diagnostic log stream
  if (diagLogStream) {
    diagLogStream.end();
    diagLogStream = null;
  }

  // Cleanup components
  if (initResult) {
    // Flush storage to ensure all state is saved
    await initResult.storage.flush();

    // Dispose status bar
    initResult.statusBar.dispose();

    // Dispose terminal manager
    await initResult.terminalManager.dispose();

    logger.info('ImmorTerm cleanup complete');
  }

  // Dispose logger last
  logger.dispose();
}
