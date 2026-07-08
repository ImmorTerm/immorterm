// Utils module exports
// Logger, screen commands, process utilities, resource extractor, settings

export { logger } from './logger';
export { screenCommands } from './screen-commands';
export type { ScreenSession } from './screen-commands';
export {
  getProjectName,
  generateWindowId,
  parseWindowIdFromSession,
  buildSessionName,
  parseProjectFromSession,
  isValidWindowId,
  getWorkspaceFolderForUri,
  getWorkspaceFolders,
} from './process';
export {
  extractResources,
  getTerminalsDir,
  getLogsDir,
  getPendingDir,
  getScriptPath,
  areResourcesExtracted,
} from './resource-extractor';
export type { ExtractionResult } from './resource-extractor';

// Settings exports
export {
  SETTINGS,
  DEFAULTS,
  getConfig,
  getConfigWithDefault,
  isSettingSet,
  updateConfig,
  // Convenience functions
  getScrollbackBuffer,
  getHistoryOnAttach,
  getTerminalRestoreDelay,
  shouldRestoreOnStartup,
  getMaxLogSizeMb,
  getLogRetainLines,
  isDebugLogEnabled,
  getCloseGracePeriod,
  shouldAutoCleanupStale,
  isStatusBarEnabled,
  getNamingPattern,
  shouldClaudeAutoResume,
  getClaudeSyncInterval,
  onSettingsChange,
  isSettingChanged,
} from './settings';
export type { SettingKey, SettingTypes } from './settings';
