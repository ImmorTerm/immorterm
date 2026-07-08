import * as vscode from 'vscode';

/**
 * Settings utility for ImmorTerm extension
 * Provides type-safe access to all configuration options
 */

/**
 * Setting key constants for type-safe access
 */
export const SETTINGS = {
  // Screen Configuration
  SCROLLBACK_BUFFER: 'scrollbackBuffer',
  HISTORY_ON_ATTACH: 'historyOnAttach',

  // Terminal Restoration
  TERMINAL_RESTORE_DELAY: 'terminalRestoreDelay',
  RESTORE_ON_STARTUP: 'restoreOnStartup',
  CLOSE_EXISTING_ON_RESTORE: 'closeExistingOnRestore',

  // Log Management
  MAX_LOG_SIZE_MB: 'maxLogSizeMb',
  LOG_RETAIN_LINES: 'logRetainLines',
  ENABLE_DEBUG_LOG: 'enableDebugLog',

  // Behavior
  CLOSE_GRACE_PERIOD: 'closeGracePeriod',
  CLOSE_ACTION: 'closeAction',
  SHELVED_SESSION_TTL: 'shelvedSessionTtl',
  AUTO_CLEANUP_STALE: 'autoCleanupStale',
  STATUS_BAR_ENABLED: 'statusBarEnabled',

  // Naming
  NAMING_PATTERN: 'namingPattern',

  // Terminal Mode
  TERMINAL_MODE: 'terminalMode',

  // Claude Integration
  CLAUDE_AUTO_RESUME: 'claudeAutoResume',
  CLAUDE_SYNC_INTERVAL: 'claudeSyncInterval',
  CLAUDE_IDLE_TIMEOUT: 'claudeIdleTimeout',
  CLAUDE_CHANNELS_ENABLED: 'claudeChannelsEnabled',
} as const;

/**
 * Type for setting keys
 */
export type SettingKey = (typeof SETTINGS)[keyof typeof SETTINGS];

/**
 * Default values for all settings
 * These match the defaults in package.json
 */
export const DEFAULTS = {
  [SETTINGS.SCROLLBACK_BUFFER]: 50000,
  [SETTINGS.HISTORY_ON_ATTACH]: 20000,
  [SETTINGS.TERMINAL_RESTORE_DELAY]: 200,
  [SETTINGS.RESTORE_ON_STARTUP]: true,
  [SETTINGS.CLOSE_EXISTING_ON_RESTORE]: true,
  [SETTINGS.MAX_LOG_SIZE_MB]: 300,
  [SETTINGS.LOG_RETAIN_LINES]: 50000,
  [SETTINGS.ENABLE_DEBUG_LOG]: false,
  [SETTINGS.CLOSE_GRACE_PERIOD]: 60000,
  [SETTINGS.CLOSE_ACTION]: 'shelve',
  [SETTINGS.SHELVED_SESSION_TTL]: 24,
  [SETTINGS.AUTO_CLEANUP_STALE]: true,
  [SETTINGS.STATUS_BAR_ENABLED]: true,
  [SETTINGS.NAMING_PATTERN]: 'immorterm-${n}',
  [SETTINGS.TERMINAL_MODE]: 'regular',
  [SETTINGS.CLAUDE_AUTO_RESUME]: true,
  [SETTINGS.CLAUDE_SYNC_INTERVAL]: 30000,
  [SETTINGS.CLAUDE_IDLE_TIMEOUT]: 48,
  [SETTINGS.CLAUDE_CHANNELS_ENABLED]: true,
} as const;

/**
 * Type mapping for setting values
 */
export interface SettingTypes {
  [SETTINGS.SCROLLBACK_BUFFER]: number;
  [SETTINGS.HISTORY_ON_ATTACH]: number;
  [SETTINGS.TERMINAL_RESTORE_DELAY]: number;
  [SETTINGS.RESTORE_ON_STARTUP]: boolean;
  [SETTINGS.CLOSE_EXISTING_ON_RESTORE]: boolean;
  [SETTINGS.MAX_LOG_SIZE_MB]: number;
  [SETTINGS.LOG_RETAIN_LINES]: number;
  [SETTINGS.ENABLE_DEBUG_LOG]: boolean;
  [SETTINGS.CLOSE_GRACE_PERIOD]: number;
  [SETTINGS.CLOSE_ACTION]: string;
  [SETTINGS.SHELVED_SESSION_TTL]: number;
  [SETTINGS.AUTO_CLEANUP_STALE]: boolean;
  [SETTINGS.STATUS_BAR_ENABLED]: boolean;
  [SETTINGS.NAMING_PATTERN]: string;
  [SETTINGS.TERMINAL_MODE]: string;
  [SETTINGS.CLAUDE_AUTO_RESUME]: boolean;
  [SETTINGS.CLAUDE_SYNC_INTERVAL]: number;
  [SETTINGS.CLAUDE_IDLE_TIMEOUT]: number;
  [SETTINGS.CLAUDE_CHANNELS_ENABLED]: boolean;
}

/**
 * Configuration namespace for ImmorTerm
 */
const CONFIG_NAMESPACE = 'immorterm';

/**
 * Gets the ImmorTerm configuration object
 */
function getConfiguration(): vscode.WorkspaceConfiguration {
  return vscode.workspace.getConfiguration(CONFIG_NAMESPACE);
}

/**
 * Type-safe configuration getter
 * @param key The setting key (use SETTINGS constants)
 * @returns The setting value with proper type, or default if not set
 */
export function getConfig<K extends SettingKey>(key: K): SettingTypes[K] {
  const config = getConfiguration();
  const defaultValue = DEFAULTS[key] as SettingTypes[K];
  return config.get<SettingTypes[K]>(key, defaultValue);
}

/**
 * Gets a configuration value with explicit default
 * @param key The setting key
 * @param defaultValue Default value to use if not set
 */
export function getConfigWithDefault<T>(key: string, defaultValue: T): T {
  const config = getConfiguration();
  return config.get<T>(key, defaultValue);
}

/**
 * Checks if a setting has been explicitly set by the user
 * @param key The setting key
 */
export function isSettingSet(key: SettingKey): boolean {
  const config = getConfiguration();
  const inspection = config.inspect(key);
  return (
    inspection?.globalValue !== undefined ||
    inspection?.workspaceValue !== undefined ||
    inspection?.workspaceFolderValue !== undefined
  );
}

/**
 * Updates a setting value
 * @param key The setting key
 * @param value The new value
 * @param target Configuration target (global, workspace, or workspace folder)
 */
export async function updateConfig<K extends SettingKey>(
  key: K,
  value: SettingTypes[K],
  target: vscode.ConfigurationTarget = vscode.ConfigurationTarget.Workspace
): Promise<void> {
  const config = getConfiguration();
  await config.update(key, value, target);
}

// ============================================================================
// Convenience functions for common settings access patterns
// ============================================================================

/**
 * Gets the scrollback buffer size for Screen sessions
 */
export function getScrollbackBuffer(): number {
  return getConfig(SETTINGS.SCROLLBACK_BUFFER);
}

/**
 * Gets the history lines to show on attach
 */
export function getHistoryOnAttach(): number {
  return getConfig(SETTINGS.HISTORY_ON_ATTACH);
}

/**
 * Gets the delay between terminal restorations
 */
export function getTerminalRestoreDelay(): number {
  return getConfig(SETTINGS.TERMINAL_RESTORE_DELAY);
}

/**
 * Checks if terminals should restore on startup
 */
export function shouldRestoreOnStartup(): boolean {
  return getConfig(SETTINGS.RESTORE_ON_STARTUP);
}

/**
 * Checks if existing terminals should be closed before restoring
 */
export function shouldCloseExistingOnRestore(): boolean {
  return getConfig(SETTINGS.CLOSE_EXISTING_ON_RESTORE);
}

/**
 * Gets the maximum log size in MB
 */
export function getMaxLogSizeMb(): number {
  return getConfig(SETTINGS.MAX_LOG_SIZE_MB);
}

/**
 * Gets the number of lines to retain when truncating logs
 */
export function getLogRetainLines(): number {
  return getConfig(SETTINGS.LOG_RETAIN_LINES);
}

/**
 * Checks if debug logging is enabled
 */
export function isDebugLogEnabled(): boolean {
  return getConfig(SETTINGS.ENABLE_DEBUG_LOG);
}

/**
 * Gets the close grace period in milliseconds
 */
export function getCloseGracePeriod(): number {
  return getConfig(SETTINGS.CLOSE_GRACE_PERIOD);
}

/**
 * Gets the close action: 'shelve' (detach) or 'kill' (destroy)
 */
export function getCloseAction(): 'shelve' | 'kill' {
  const action = getConfig(SETTINGS.CLOSE_ACTION);
  return action === 'kill' ? 'kill' : 'shelve';
}

/**
 * Gets the shelved session TTL in hours
 */
export function getShelvedSessionTtl(): number {
  return getConfig(SETTINGS.SHELVED_SESSION_TTL);
}

/**
 * Checks if auto cleanup of stale sessions is enabled
 */
export function shouldAutoCleanupStale(): boolean {
  return getConfig(SETTINGS.AUTO_CLEANUP_STALE);
}

/**
 * Checks if the status bar should be shown
 */
export function isStatusBarEnabled(): boolean {
  return getConfig(SETTINGS.STATUS_BAR_ENABLED);
}

/**
 * Gets the naming pattern for terminals
 */
export function getNamingPattern(): string {
  return getConfig(SETTINGS.NAMING_PATTERN);
}

/**
 * Checks if Claude auto-resume is enabled
 */
export function shouldClaudeAutoResume(): boolean {
  return getConfig(SETTINGS.CLAUDE_AUTO_RESUME);
}

/**
 * Gets the Claude sync interval in milliseconds
 */
export function getClaudeSyncInterval(): number {
  return getConfig(SETTINGS.CLAUDE_SYNC_INTERVAL);
}

/**
 * Gets the Claude idle timeout in hours (0 = disabled)
 */
export function getClaudeIdleTimeout(): number {
  return getConfig(SETTINGS.CLAUDE_IDLE_TIMEOUT);
}

/**
 * Gets the terminal mode: which backend(s) to auto-restore on startup.
 * 'ai' = the ImmorTerm Rust terminal (the only terminal). 'regular'/'both'
 * are legacy values kept readable so pre-existing configs keep restoring
 * their old screen sessions.
 */
export function getTerminalMode(): 'regular' | 'ai' | 'both' {
  const mode = getConfig(SETTINGS.TERMINAL_MODE);
  // Validate against known values to guard against invalid user input
  if (mode === 'ai' || mode === 'both') return mode;
  return 'regular';
}

/**
 * Saves the workspace settings.json file if it has unsaved changes.
 * VS Code's Configuration API refuses to write when the file is dirty,
 * throwing "Unable to write into workspace settings because the file
 * has unsaved changes." This helper resolves that by saving first.
 */
export async function saveDirtyWorkspaceSettings(): Promise<void> {
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  if (!workspaceFolder) return;

  const path = await import('path');
  const settingsPath = path.join(workspaceFolder.uri.fsPath, '.vscode', 'settings.json');
  const dirtyDoc = vscode.workspace.textDocuments.find(
    d => d.uri.fsPath === settingsPath && d.isDirty
  );
  if (dirtyDoc) {
    await dirtyDoc.save();
  }
}

/**
 * Creates a settings change listener
 * @param callback Function to call when settings change
 * @returns Disposable to unsubscribe
 */
export function onSettingsChange(
  callback: (e: vscode.ConfigurationChangeEvent) => void
): vscode.Disposable {
  return vscode.workspace.onDidChangeConfiguration((e) => {
    if (e.affectsConfiguration(CONFIG_NAMESPACE)) {
      callback(e);
    }
  });
}

/**
 * Checks if a specific setting was changed
 * @param event The configuration change event
 * @param key The setting key to check
 */
export function isSettingChanged(
  event: vscode.ConfigurationChangeEvent,
  key: SettingKey
): boolean {
  return event.affectsConfiguration(`${CONFIG_NAMESPACE}.${key}`);
}
