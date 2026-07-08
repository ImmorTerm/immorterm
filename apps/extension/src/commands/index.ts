// Commands module exports
// All command handlers for ImmorTerm

// Reconcile - register terminal in workspace storage
export { reconcileTerminal, type ReconcileResult } from './reconcile';

// Cleanup - remove stale terminal entries + session archive lifecycle
export { cleanupStaleTerminals, type CleanupResult, archiveSessionByWindowId, unarchiveSessionDir } from './cleanup';

// Forget - remove single terminal
export { forgetTerminal, type ForgetResult } from './forget';

// Forget All - remove all terminals for project
export { forgetAllTerminals, type ForgetAllResult } from './forget-all';

// Log Cleanup - manage log file sizes
export { cleanupLogs, type LogCleanupResult } from './log-cleanup';

// Kill All - kill all screen sessions for project
export { killAllScreenSessions, type KillAllResult } from './kill-all';

// Rename Terminal - rename via VS Code input box (Ctrl+Shift+R)
export { renameTerminal, type RenameTerminalResult } from './rename-terminal';

// Toggle Title Lock - lock/unlock terminal title (Ctrl+Shift+L)
export { toggleTitleLock, type ToggleTitleLockResult } from './toggle-title-lock';

// Reattach Terminal - reconnect to a shelved terminal
export { reattachTerminal, type ReattachResult } from './reattach';
