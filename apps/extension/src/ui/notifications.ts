import * as vscode from 'vscode';
import { logger } from '../utils/logger';

/**
 * Notifications utility for ImmorTerm user feedback
 * Provides consistent notification patterns across the extension
 */
export const notifications = {
  /**
   * Shows notification when a terminal is forgotten
   * @param name The terminal name that was forgotten
   */
  showTerminalForget(name: string): void {
    vscode.window.showInformationMessage(
      `ImmorTerm: Forgot terminal "${name}". Session terminated.`
    );
    logger.info('Showed terminal forget notification:', name);
  },

  /**
   * Shows notification when all terminals are forgotten
   * @param count The number of terminals forgotten
   */
  showAllTerminalsForgotten(count: number): void {
    vscode.window.showInformationMessage(
      `ImmorTerm: Forgot ${count} terminal${count !== 1 ? 's' : ''}. All sessions terminated.`
    );
    logger.info('Showed all terminals forgotten notification:', count);
  },

  /**
   * Shows notification when cleanup completes
   * @param count The number of stale sessions cleaned up
   */
  showCleanupComplete(count: number): void {
    if (count === 0) {
      vscode.window.showInformationMessage(
        'ImmorTerm: No stale sessions found. Everything is clean!'
      );
    } else {
      vscode.window.showInformationMessage(
        `ImmorTerm: Cleaned up ${count} stale session${count !== 1 ? 's' : ''}.`
      );
    }
    logger.info('Showed cleanup complete notification:', count);
  },

  /**
   * Shows notification when Screen sessions are killed
   * @param count The number of sessions killed
   */
  showSessionsKilled(count: number): void {
    vscode.window.showInformationMessage(
      `ImmorTerm: Killed ${count} session${count !== 1 ? 's' : ''}.`
    );
    logger.info('Showed sessions killed notification:', count);
  },

  /**
   * Shows notification when terminals are restored
   * @param count The number of terminals restored
   */
  showTerminalsRestored(count: number): void {
    if (count > 0) {
      vscode.window.showInformationMessage(
        `ImmorTerm: Restored ${count} terminal${count !== 1 ? 's' : ''}.`
      );
      logger.info('Showed terminals restored notification:', count);
    }
  },

  /**
   * Shows notification when sync completes
   */
  showSyncComplete(): void {
    vscode.window.showInformationMessage('ImmorTerm: Terminal names synced.');
    logger.info('Showed sync complete notification');
  },

  /**
   * Shows an error message with optional View Logs action
   * @param message The error message to display
   * @param showLogs Whether to show the View Logs action (default: true)
   */
  async showError(message: string, showLogs: boolean = true): Promise<void> {
    const actions = showLogs ? ['View Logs', 'Dismiss'] : ['Dismiss'];

    const result = await vscode.window.showErrorMessage(
      `ImmorTerm: ${message}`,
      ...actions
    );

    if (result === 'View Logs') {
      logger.show();
    }

    logger.error('Showed error notification:', message);
  },

  /**
   * Shows a confirmation prompt
   * @param message The confirmation message
   * @param confirmText The confirm button text (default: "Yes")
   * @returns true if user confirmed
   */
  async showConfirmation(message: string, confirmText: string = 'Yes'): Promise<boolean> {
    const result = await vscode.window.showWarningMessage(
      `ImmorTerm: ${message}`,
      { modal: true },
      confirmText,
      'Cancel'
    );

    return result === confirmText;
  },

  /**
   * Shows a progress notification for long-running operations
   * @param title The progress title
   * @param task The async task to run
   * @returns The result of the task
   */
  async withProgress<T>(
    title: string,
    task: (
      progress: vscode.Progress<{ message?: string; increment?: number }>,
      token: vscode.CancellationToken
    ) => Promise<T>
  ): Promise<T> {
    return vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: `ImmorTerm: ${title}`,
        cancellable: false,
      },
      task
    );
  },
};

export default notifications;
