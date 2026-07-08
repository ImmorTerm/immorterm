/**
 * Project Identity
 *
 * Provides stable, unique project IDs that persist across:
 * - Folder renames (if git remote exists)
 * - VS Code restarts
 * - Different machines (same git repo)
 *
 * Used to namespace memory collections per-project.
 */

import * as fs from 'fs';
import * as path from 'path';
import { execSync } from 'child_process';

/**
 * Get the repository name from git remote origin URL.
 * Examples:
 *   git@github.com:user/repo.git -> user-repo
 *   https://github.com/user/repo.git -> user-repo
 *   https://github.com/user/repo -> user-repo
 *
 * @param workspacePath Path to the workspace folder
 * @returns Repository name or null if not a git repo
 */
function getGitRemoteRepoName(workspacePath: string): string | null {
  try {
    // Note: Using execSync with hardcoded command - no user input, safe from injection
    const remoteUrl = execSync('git config --get remote.origin.url', {
      cwd: workspacePath,
      encoding: 'utf8',
      timeout: 5000,
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim();

    if (!remoteUrl) return null;

    // Extract user/repo from various URL formats
    // git@github.com:user/repo.git
    const sshMatch = remoteUrl.match(/[:/]([^/]+)\/([^/]+?)(?:\.git)?$/);
    if (sshMatch) {
      return `${sshMatch[1]}-${sshMatch[2]}`.toLowerCase();
    }

    // https://github.com/user/repo.git
    const httpsMatch = remoteUrl.match(/\/([^/]+)\/([^/]+?)(?:\.git)?$/);
    if (httpsMatch) {
      return `${httpsMatch[1]}-${httpsMatch[2]}`.toLowerCase();
    }

    return null;
  } catch {
    // Not a git repo or git not available
    return null;
  }
}

/**
 * Read project ID from .claude/project-id file if it exists.
 *
 * @param workspacePath Path to the workspace folder
 * @returns Saved project ID or null if not found
 */
function readProjectIdFile(workspacePath: string): string | null {
  const projectIdPath = path.join(workspacePath, '.claude', 'project-id');

  try {
    if (fs.existsSync(projectIdPath)) {
      const content = fs.readFileSync(projectIdPath, 'utf8').trim();
      // Validate: only alphanumeric and hyphens
      if (/^[a-z0-9-]+$/.test(content)) {
        return content;
      }
    }
  } catch {
    // File doesn't exist or can't be read
  }

  return null;
}

/**
 * Save project ID to .claude/project-id file.
 * Creates .claude directory if it doesn't exist.
 *
 * @param workspacePath Path to the workspace folder
 * @param projectId The project ID to save
 */
function writeProjectIdFile(workspacePath: string, projectId: string): void {
  const claudeDir = path.join(workspacePath, '.claude');
  const projectIdPath = path.join(claudeDir, 'project-id');

  try {
    // Create .claude directory if it doesn't exist
    if (!fs.existsSync(claudeDir)) {
      fs.mkdirSync(claudeDir, { recursive: true });
    }

    fs.writeFileSync(projectIdPath, projectId, 'utf8');
  } catch (error) {
    // Log but don't throw - project ID can be regenerated
    console.error('[memory] Failed to save project ID:', error);
  }
}

/**
 * Sanitize a string for use as a project ID.
 * Removes special characters, converts to lowercase.
 *
 * @param name Raw name to sanitize
 * @returns Sanitized ID (lowercase, alphanumeric with hyphens)
 */
function sanitizeProjectId(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')  // Replace non-alphanumeric with hyphens
    .replace(/^-+|-+$/g, '')       // Trim leading/trailing hyphens
    .slice(0, 50);                 // Limit length
}

/**
 * Get a stable, unique project ID for the workspace.
 *
 * Resolution order (most stable first):
 * 1. Git remote origin (survives folder renames)
 * 2. Saved .claude/project-id file (persists across sessions)
 * 3. Folder name (fallback, saved to file for consistency)
 *
 * @param workspacePath Path to the workspace folder
 * @returns Stable project ID (lowercase, alphanumeric with hyphens)
 *
 * @example
 * // Git repo at github.com/user/my-app
 * getStableProjectId('/path/to/my-app') // Returns 'user-my-app'
 *
 * // Non-git project
 * getStableProjectId('/path/to/My Project') // Returns 'my-project'
 */
export function getStableProjectId(workspacePath: string): string {
  // 1. Try git remote (most stable - survives folder renames)
  const gitRemote = getGitRemoteRepoName(workspacePath);
  if (gitRemote) {
    return gitRemote;
  }

  // 2. Try saved .claude/project-id file
  const savedId = readProjectIdFile(workspacePath);
  if (savedId) {
    return savedId;
  }

  // 3. Create new ID from folder name, save to file for persistence
  const folderName = path.basename(workspacePath);
  const folderId = sanitizeProjectId(folderName) || 'unnamed-project';

  // Save for future sessions
  writeProjectIdFile(workspacePath, folderId);

  return folderId;
}

export default {
  getStableProjectId,
};
