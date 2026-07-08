// Terminal module exports
// Manager, restoration, naming, and screen integration

export { TerminalManager } from './manager';

export {
  restoreTerminals,
  restoreTerminalsWithDelay,
  isRestoreOnStartupEnabled,
  type RestorationResult,
} from './restoration';

export {
  createTerminalWithScreen,
  createNewImmorTerminal,
  createStandardTerminal,
  isImmorTermTerminal,
  setScreenAvailable,
  isScreenAvailable,
  type CreateTerminalOptions,
} from './screen-integration';

export {
  generateNextName,
  syncTerminalName,
  checkAndSyncNameChange,
  trackTerminalName,
  getLastKnownName,
  autoRenameIfNeeded,
  isRawWindowIdFormat,
} from './naming';
