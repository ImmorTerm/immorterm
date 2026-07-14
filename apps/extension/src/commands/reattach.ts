import * as vscode from 'vscode';
import { logger } from '../utils/logger';
import { getShelvedSessions, updateSessionStatus, removeTerminalFromRegistry, removeSessionStatus } from '../registry-client';
import { screenCommands } from '../utils/screen-commands';
import { createTerminalWithScreen } from '../terminal/screen-integration';
import { TerminalManager } from '../terminal/manager';
import { getScriptsDir, getTerminalsDir, getLogsDir } from '../utils/resource-extractor';
import { markAsRestored } from '../extension';
import { unarchiveSessionDir, findSessionDir } from './cleanup';
import type { ImmorTermViewProvider } from '../gpu-terminal';
import * as fs from 'fs/promises';
import * as path from 'path';

export interface ReattachResult {
  success: boolean;
  windowId: string;
  displayName: string;
}

// Button definitions with Codicon icons
const restoreButton: vscode.QuickInputButton = {
  iconPath: new vscode.ThemeIcon('debug-start'),
  tooltip: 'Restore session',
};
const summaryButton: vscode.QuickInputButton = {
  iconPath: new vscode.ThemeIcon('info'),
  tooltip: 'View session summary',
};
const deleteButton: vscode.QuickInputButton = {
  iconPath: new vscode.ThemeIcon('trash'),
  tooltip: 'Delete session',
};

interface ShelvedQuickPickItem extends vscode.QuickPickItem {
  entry: ReturnType<typeof getShelvedSessions>[number];
}

/**
 * Shows a QuickPick of shelved sessions with action buttons:
 * - Restore (play): reattach the session
 * - Summary (info): show session summary modal
 * - Delete (trash): permanently remove the session
 *
 * Clicking the row itself does nothing — only buttons trigger actions.
 */
export async function reattachTerminal(
  manager: TerminalManager,
  aiProvider?: ImmorTermViewProvider | null,
): Promise<ReattachResult | undefined> {
  const shelved = getShelvedSessions();

  if (shelved.length === 0) {
    vscode.window.showInformationMessage('ImmorTerm: No shelved terminals to reattach');
    return undefined;
  }

  return new Promise<ReattachResult | undefined>((resolve) => {
    const qp = vscode.window.createQuickPick<ShelvedQuickPickItem>();
    qp.title = 'ImmorTerm: Shelved Sessions';
    qp.placeholder = 'Use the buttons to restore, view summary, or delete';
    qp.matchOnDescription = true;
    qp.matchOnDetail = true;

    const buildItems = (): ShelvedQuickPickItem[] => {
      const current = getShelvedSessions();
      return current.map(entry => {
        const age = Math.floor((Date.now() / 1000 - (entry.shelved_at || 0)) / 60);
        const ageStr = age < 60 ? `${age}m ago` : `${Math.floor(age / 60)}h ago`;
        const typeLabel = entry.session_type === 'ai' ? ' [AI]' : '';
        return {
          label: entry.display_name || entry.name,
          description: `${typeLabel} shelved ${ageStr}`,
          detail: `Window ID: ${entry.window_id}`,
          buttons: [restoreButton, summaryButton, deleteButton],
          entry,
        };
      });
    };

    qp.items = buildItems();

    // Clicking the row itself does nothing (no onDidAccept handler that resolves)
    qp.onDidAccept(() => {
      // Intentionally empty — only buttons trigger actions
    });

    qp.onDidTriggerItemButton(async (e) => {
      const { item, button } = e;
      const { entry } = item;
      const windowId = entry.window_id;
      const displayName = entry.display_name || entry.name;

      if (button === restoreButton) {
        qp.hide();
        const result = await doReattach(entry, manager, aiProvider);
        resolve(result);
      } else if (button === summaryButton) {
        // Show summary without closing the QuickPick
        if (aiProvider) {
          await aiProvider.showSessionSummary(windowId);
        } else {
          vscode.window.showInformationMessage(`Session summary not available (AI provider not loaded)`);
        }
      } else if (button === deleteButton) {
        const confirm = await vscode.window.showWarningMessage(
          `Delete shelved session "${displayName}"? This cannot be undone.`,
          { modal: true },
          'Delete',
        );
        if (confirm === 'Delete') {
          await deleteShelvedSession(windowId, manager);
          // Refresh items list after deletion
          const remaining = buildItems();
          if (remaining.length === 0) {
            qp.hide();
            vscode.window.showInformationMessage('ImmorTerm: No more shelved sessions');
            resolve(undefined);
          } else {
            qp.items = remaining;
          }
        }
      }
    });

    qp.onDidHide(() => {
      qp.dispose();
      resolve(undefined);
    });

    qp.show();
  });
}

/**
 * Reattach a shelved session (AI or regular).
 */
async function doReattach(
  entry: ReturnType<typeof getShelvedSessions>[number],
  manager: TerminalManager,
  aiProvider?: ImmorTermViewProvider | null,
): Promise<ReattachResult> {
  const windowId = entry.window_id;
  const displayName = entry.display_name || entry.name;
  const projectName = manager.getProjectName();
  const screenSession = `${projectName}-${windowId}`;

  // AI sessions: delegate to ImmorTermViewProvider
  if (entry.session_type === 'ai') {
    if (!aiProvider) {
      vscode.window.showWarningMessage('ImmorTerm: AI terminal provider not available — cannot reattach');
      return { success: false, windowId, displayName };
    }
    const ok = await aiProvider.reattachSession(windowId);
    if (ok) {
      logger.info(`Reattached shelved AI terminal: ${displayName} (${windowId})`);
      vscode.commands.executeCommand('immorterm.terminalView.focus');
    } else {
      vscode.window.showWarningMessage(`ImmorTerm: Failed to reattach AI session "${displayName}"`);
    }
    return { success: ok, windowId, displayName };
  }

  // Regular session: verify screen session is alive
  const exists = await screenCommands.sessionExists(screenSession);
  if (!exists) {
    vscode.window.showWarningMessage(
      `ImmorTerm: Session "${displayName}" is no longer alive (session gone)`
    );
    updateSessionStatus(windowId, 'dead');
    return { success: false, windowId, displayName };
  }

  const wsFolder = vscode.workspace.workspaceFolders?.[0];
  const scriptsPath = getScriptsDir();
  const terminalsDir = wsFolder ? getTerminalsDir(wsFolder) : undefined;

  if (wsFolder) {
    const logsDir = getLogsDir(wsFolder);
    await unarchiveSessionDir(logsDir, windowId);
  }

  const terminal = createTerminalWithScreen({
    name: displayName,
    windowId,
    scriptsPath,
    terminalsDir,
    cwd: wsFolder?.uri.fsPath,
    isRestoration: true,
    isReattach: true,
    titleLocked: entry.title_locked,
    claudeResumeId: entry.claude_resume_id,
  });

  manager.trackTerminal(terminal, windowId);
  markAsRestored(windowId, displayName);

  await manager.getStorage().addTerminal({
    windowId,
    name: displayName,
    screenSession,
    createdAt: (entry.created_at || 0) * 1000,
    lastAttached: Date.now(),
    titleLocked: entry.title_locked,
    theme: entry.theme,
  });

  updateSessionStatus(windowId, 'active');
  terminal.show(false);
  logger.info(`Reattached shelved terminal: ${displayName} (${windowId})`);
  return { success: true, windowId, displayName };
}

/**
 * Delete a shelved session permanently — remove from registry, delete archived logs.
 */
async function deleteShelvedSession(windowId: string, manager: TerminalManager): Promise<void> {
  logger.info(`Deleting shelved session: ${windowId}`);

  // Remove from registry + session-status (permanent delete)
  removeTerminalFromRegistry(windowId);
  removeSessionStatus(windowId);

  // Remove from workspace storage (if present)
  await manager.getStorage().removeTerminal(windowId);

  // Delete archived log directory
  const wsFolder = vscode.workspace.workspaceFolders?.[0];
  if (wsFolder) {
    const logsDir = getLogsDir(wsFolder);
    const archiveDir = path.join(logsDir, 'archive');
    const sessionDir = await findSessionDir(archiveDir, windowId);
    if (sessionDir) {
      try {
        await fs.rm(sessionDir, { recursive: true, force: true });
        logger.info(`Deleted archived logs: ${sessionDir}`);
      } catch (err) {
        logger.warn(`Failed to delete archived logs for ${windowId}:`, err);
      }
    }
  }

  logger.info(`Deleted shelved session: ${windowId}`);
}
