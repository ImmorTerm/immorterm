import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import { logger } from '../utils/logger';
import { getScrollbackBuffer, getHistoryOnAttach } from '../utils/settings';

/** Module-level flag for Screen availability (set by activation) */
let screenAvailableFlag = true;

/**
 * Sets the global Screen availability flag
 * Called during activation after Screen detection
 */
export function setScreenAvailable(available: boolean): void {
  screenAvailableFlag = available;
  logger.debug('Screen availability set to:', available);
}

/**
 * Gets the current Screen availability status
 */
export function isScreenAvailable(): boolean {
  return screenAvailableFlag;
}

/**
 * Options for creating a terminal with Screen integration
 */
export interface CreateTerminalOptions {
  /** Display name for the terminal tab */
  name: string;
  /** Unique window identifier (used in Screen session name) */
  windowId: string;
  /** Path to the scripts directory (~/.immorterm/scripts/) */
  scriptsPath: string;
  /** Path to the per-project terminals directory (<project>/.immorterm/terminals/) for renames, logs */
  terminalsDir?: string;
  /** Working directory for the terminal */
  cwd?: string;
  /** Whether this is a restoration (vs new terminal creation) */
  isRestoration?: boolean;
  /** Claude session ID if this terminal has an active Claude session */
  claudeSessionId?: string;
  /** Whether the title is locked (user-renamed). Defaults to false (unlocked). */
  titleLocked?: boolean;
  /** Whether this is a reattach of a shelved session (skip scrollback clearing) */
  isReattach?: boolean;
  /** Claude session ID to auto-resume on reattach (from hibernated session) */
  claudeResumeId?: string;
}

/** Cached result of immorterm-ai binary lookup (null = not searched yet) */
let cachedAiBinary: string | null | undefined;

/**
 * Finds the immorterm-ai binary for structured logging sidecar.
 * Synchronous check — screen-auto also has its own fallback resolution.
 */
function findAiBinary(): string {
  if (cachedAiBinary !== undefined) return cachedAiBinary || '';
  const home = process.env.HOME || '~';
  const candidates = [
    path.join(home, 'Development', 'immorterm', 'target', 'release', 'immorterm-ai'),
    path.join(home, 'Development', 'immorterm', 'target', 'debug', 'immorterm-ai'),
    '/usr/local/bin/immorterm-ai',
  ];
  for (const loc of candidates) {
    if (fs.existsSync(loc)) {
      cachedAiBinary = loc;
      logger.debug('Found immorterm-ai binary:', loc);
      return loc;
    }
  }
  cachedAiBinary = null;
  logger.debug('immorterm-ai binary not found in standard locations (screen-auto will try PATH)');
  return '';
}

/**
 * Creates a VS Code terminal that uses screen-auto as its shell
 *
 * The terminal is configured with:
 * - shellPath: /bin/zsh (or bash as fallback)
 * - shellArgs: Executes screen-auto with windowId and name
 *
 * screen-auto handles:
 * - Creating new Screen sessions for new terminals
 * - Attaching to existing Screen sessions for restored terminals
 * - Setting up logging and scrollback
 *
 * If Screen is not available, creates a standard terminal (graceful degradation)
 *
 * @param options Terminal creation options
 * @returns The created VS Code Terminal instance
 */
export function createTerminalWithScreen(options: CreateTerminalOptions): vscode.Terminal {
  const { name, windowId, scriptsPath, terminalsDir, cwd, isRestoration = false, claudeSessionId, titleLocked, isReattach = false, claudeResumeId } = options;

  // Graceful degradation: if Screen is not available, create standard terminal
  if (!screenAvailableFlag) {
    logger.warn('Screen not available, creating standard terminal (no persistence):', name);
    return createStandardTerminal(name, cwd);
  }

  // Determine the shell to use
  const shellPath = getShellPath();

  // Build the command to execute screen-auto
  const screenAutoPath = path.join(scriptsPath, 'screen-auto');

  // For restoration: pass windowId and display name to screen-auto
  // For new terminals: screen-auto generates its own windowId (called without args)
  let shellArgs: string[];

  if (isRestoration) {
    // Restored terminal: provide windowId and display name
    // screen-auto will attach to existing session or create new one with this ID
    shellArgs = ['-c', `exec "${screenAutoPath}" "${windowId}" "${name}"`];
  } else {
    // New terminal: let screen-auto generate the windowId
    // Note: For v3, we generate windowId in TypeScript and pass it
    shellArgs = ['-c', `exec "${screenAutoPath}" "${windowId}" "${name}"`];
  }

  // Get configured screen binary
  const screenBinary = vscode.workspace.getConfiguration('immorterm').get<string>('screenBinary', 'immorterm');

  // titleLocked is the source of truth for whether OSC sequences can rename the tab.
  // When locked (user-renamed): set TerminalOptions.name to protect from OSC override.
  // When unlocked (default): omit name so ${sequence} template shows OSC titles.
  const locked = titleLocked ?? false;

  logger.debug('Creating terminal with Screen:', {
    name,
    windowId,
    shellPath,
    shellArgs,
    isRestoration,
    claudeSessionId: claudeSessionId ? 'exists' : 'none',
    locked,
  });

  const renamesDir = path.join(terminalsDir || scriptsPath, 'renames');
  const scrollback = getScrollbackBuffer();
  const historyLines = getHistoryOnAttach();
  const terminalOptions: vscode.TerminalOptions = {
    shellPath,
    shellArgs,
    cwd,
    env: {
      IMMORTERM_WINDOW_ID: windowId,
      IMMORTERM_DISPLAY_NAME: name,
      IMMORTERM_SCREEN_BINARY: screenBinary,
      IMMORTERM_AI_BINARY: findAiBinary(),
      IMMORTERM_RENAMES_DIR: renamesDir,
      IMMORTERM_LOG_DIR: path.join(terminalsDir || scriptsPath, 'logs'),
      IMMORTERM_TITLE_LOCKED: locked ? '1' : '0',
      IMMORTERM_REATTACH: isReattach ? '1' : '0',
      IMMORTERM_CLAUDE_RESUME_ID: claudeResumeId || '',
      SCREEN_SCROLLBACK: String(scrollback),
      SCREEN_HISTORY_LINES: String(historyLines),
    },
  };

  if (locked) {
    // Locked: set name to protect from OSC override (user explicitly renamed this tab)
    terminalOptions.name = name;
    logger.debug(`Setting VS Code name property for locked terminal: "${name}"`);
  }

  const terminal = vscode.window.createTerminal(terminalOptions);

  return terminal;
}

/**
 * Creates a standard VS Code terminal without Screen integration
 * Used for graceful degradation when Screen is not available
 *
 * @param name Display name for the terminal
 * @param cwd Working directory
 * @returns The created VS Code Terminal instance
 */
export function createStandardTerminal(name: string, cwd?: string): vscode.Terminal {
  logger.debug('Creating standard terminal (no Screen):', name);

  return vscode.window.createTerminal({
    name,
    cwd,
  });
}

/**
 * Gets the appropriate shell path for the current platform
 * Prefers zsh on macOS, falls back to bash
 *
 * @returns Path to the shell executable
 */
function getShellPath(): string {
  const platform = process.platform;

  if (platform === 'darwin') {
    // macOS: prefer zsh (default since Catalina)
    return '/bin/zsh';
  } else if (platform === 'linux') {
    // Linux: check for zsh, fall back to bash
    // Note: In production, we'd check if zsh exists, but for simplicity use bash
    return '/bin/bash';
  } else {
    // Windows (WSL) or other: use bash
    return '/bin/bash';
  }
}

/**
 * Creates a new ImmorTerm terminal with a generated windowId
 *
 * This is a convenience function that combines ID generation with terminal creation.
 * For most cases, the caller should generate the windowId separately and use
 * createTerminalWithScreen directly.
 *
 * @param scriptsPath Path to the scripts directory
 * @param name Display name for the terminal
 * @param cwd Working directory
 * @returns Object containing the terminal and its windowId
 */
export function createNewImmorTerminal(
  scriptsPath: string,
  name: string,
  cwd?: string
): { terminal: vscode.Terminal; windowId: string } {
  // Import here to avoid circular dependency
  const { generateWindowId } = require('../utils/process');

  const windowId = generateWindowId();

  const terminal = createTerminalWithScreen({
    name,
    windowId,
    scriptsPath,
    cwd,
    isRestoration: false,
  });

  return { terminal, windowId };
}

/**
 * Checks if a terminal appears to be an ImmorTerm terminal
 * Based on environment variables or naming convention
 *
 * @param terminal The terminal to check
 * @returns true if this appears to be an ImmorTerm terminal
 */
export function isImmorTermTerminal(terminal: vscode.Terminal): boolean {
  // We can't directly check environment variables of an existing terminal
  // Instead, we rely on the TerminalManager to track which terminals are ours
  // This function is a placeholder for potential future heuristics

  // Check if the terminal name matches our naming pattern
  const namePattern = /^[a-z0-9-]+-\d+$/;
  return namePattern.test(terminal.name);
}

export default {
  createTerminalWithScreen,
  createNewImmorTerminal,
  createStandardTerminal,
  isImmorTermTerminal,
  setScreenAvailable,
  isScreenAvailable,
};
