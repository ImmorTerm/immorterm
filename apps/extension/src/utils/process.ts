import * as vscode from 'vscode';
import * as crypto from 'crypto';

/**
 * Process utilities for ImmorTerm extension
 * Handles project naming, window ID generation, and session parsing
 */

/**
 * Gets the current project name from the workspace folder
 * Returns lowercase folder name for consistency in Screen session naming
 *
 * @param workspaceFolder Optional specific workspace folder, defaults to first folder
 * @returns Lowercase project name, or 'unknown' if no workspace
 */
export function getProjectName(workspaceFolder?: vscode.WorkspaceFolder): string {
  const folder = workspaceFolder ?? vscode.workspace.workspaceFolders?.[0];

  if (!folder) {
    return 'unknown';
  }

  // Get the folder name (last segment of the path)
  const folderName = folder.name;

  // Convert to lowercase and replace spaces/special chars with hyphens
  return folderName
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '');
}

/**
 * Generates a unique window ID for a new terminal
 * Format: "$$-{random8}" where $$ is the current process PID
 * and random8 is an 8-character random hex string
 *
 * The PID prefix helps identify which VS Code window created the terminal,
 * while the random suffix ensures uniqueness within the same window.
 *
 * @returns Unique window ID string
 */
export function generateWindowId(): string {
  const pid = process.pid;
  const random = crypto.randomBytes(4).toString('hex'); // 8 hex chars
  return `${pid}-${random}`;
}

/**
 * Parses a window ID from a Screen session name
 * Session names follow the pattern: "project-windowId"
 *
 * @param sessionName The full session name (e.g., "myproject-12345-abc12345")
 * @returns The window ID portion, or null if parsing fails
 */
export function parseWindowIdFromSession(sessionName: string): string | null {
  // Session format: "projectname-pid-random"
  // We need to extract "pid-random" (the windowId)

  const parts = sessionName.split('-');

  // Need at least 3 parts: project, pid, random
  if (parts.length < 3) {
    return null;
  }

  // The window ID is everything after the first hyphen
  // This handles project names that might contain hyphens
  const firstHyphenIndex = sessionName.indexOf('-');
  if (firstHyphenIndex === -1) {
    return null;
  }

  return sessionName.substring(firstHyphenIndex + 1);
}

/**
 * Builds a Screen session name from project and window ID
 *
 * @param projectName The project name (lowercase)
 * @param windowId The unique window identifier
 * @returns Full session name for Screen
 */
export function buildSessionName(projectName: string, windowId: string): string {
  return `${projectName}-${windowId}`;
}

/**
 * Extracts the project name from a session name
 *
 * @param sessionName The full session name
 * @returns The project name portion, or null if parsing fails
 */
export function parseProjectFromSession(sessionName: string): string | null {
  const hyphenIndex = sessionName.indexOf('-');
  if (hyphenIndex === -1) {
    return null;
  }
  return sessionName.substring(0, hyphenIndex);
}

/**
 * Validates that a window ID matches expected format
 *
 * @param windowId The window ID to validate
 * @returns true if valid, false otherwise
 */
export function isValidWindowId(windowId: string): boolean {
  // Format: "pid-random" where pid is numeric and random is 8 hex chars
  const pattern = /^\d+-[a-f0-9]{8}$/;
  return pattern.test(windowId);
}

/**
 * Gets the workspace folder for a given URI
 *
 * @param uri The URI to find the workspace folder for
 * @returns The workspace folder, or undefined if not found
 */
export function getWorkspaceFolderForUri(uri: vscode.Uri): vscode.WorkspaceFolder | undefined {
  return vscode.workspace.getWorkspaceFolder(uri);
}

/**
 * Gets all workspace folders
 *
 * @returns Array of workspace folders, or empty array if none
 */
export function getWorkspaceFolders(): readonly vscode.WorkspaceFolder[] {
  return vscode.workspace.workspaceFolders ?? [];
}

export default {
  getProjectName,
  generateWindowId,
  parseWindowIdFromSession,
  buildSessionName,
  parseProjectFromSession,
  isValidWindowId,
  getWorkspaceFolderForUri,
  getWorkspaceFolders,
};
