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
 * Directories probed for a saved project-id, in order. `.immorterm/` is the
 * canonical home; `.claude/` is where it used to live and is still read,
 * because relocating it would change the slug — and therefore the plans and
 * tasks directories — under every project that already has one.
 *
 * Must stay in step with the daemon's `project_id_from_file` (mcp.rs) and the
 * hub's `read_project_id_file` (routes/project_id.rs).
 */
const PROJECT_ID_DIRS = ['.immorterm', '.claude'] as const;

/**
 * Read the saved project ID if one exists, preferring the canonical location.
 *
 * @param workspacePath Path to the workspace folder
 * @returns Saved project ID or null if not found
 */
function readProjectIdFile(workspacePath: string): string | null {
  for (const dir of PROJECT_ID_DIRS) {
    const projectIdPath = path.join(workspacePath, dir, 'project-id');
    try {
      if (!fs.existsSync(projectIdPath)) continue;
      const content = fs.readFileSync(projectIdPath, 'utf8').trim();
      if (content) {
        // Sanitize instead of reject — MUST match the daemon's
        // sanitize_project_id (mcp.rs) exactly, or the extension reads a
        // different ~/.immorterm/plans/<id>/ than the daemon writes and the
        // Plans view stays empty. Rules: lowercase, each non-alphanumeric
        // char → '-', trim '-', cap 50, 'unnamed-project' fallback.
        const sanitized = content
          .toLowerCase()
          .replace(/[^a-z0-9]/g, '-')
          .replace(/^-+|-+$/g, '');
        return sanitized ? sanitized.slice(0, 50) : 'unnamed-project';
      }
    } catch {
      // Unreadable — try the next candidate.
    }
  }

  return null;
}

/**
 * Save project ID to `<workspace>/.immorterm/project-id`.
 *
 * Writes only the canonical location. An existing `.claude/project-id` is left
 * exactly where it is — readProjectIdFile still finds it, so no project's slug
 * moves; over time only new projects stop growing a `.claude/` they may never
 * otherwise need (a Codex-only user has no other reason for one).
 *
 * @param workspacePath Path to the workspace folder
 * @param projectId The project ID to save
 */
function writeProjectIdFile(workspacePath: string, projectId: string): void {
  const stateDir = path.join(workspacePath, '.immorterm');
  const projectIdPath = path.join(stateDir, 'project-id');

  try {
    if (!fs.existsSync(stateDir)) {
      fs.mkdirSync(stateDir, { recursive: true });
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
