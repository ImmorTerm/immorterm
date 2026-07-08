import { exec, execFile, execFileSync } from 'child_process';
import { promisify } from 'util';
import * as vscode from 'vscode';
import { logger } from './logger';
import { auditedKill } from './kill-audit';

const execAsync = promisify(exec);
const execFileAsync = promisify(execFile);

/**
 * Gets the configured screen binary path from settings
 * @returns The screen binary path (default: 'immorterm')
 */
function getScreenBinary(): string {
  const config = vscode.workspace.getConfiguration('immorterm');
  return config.get<string>('screenBinary', 'immorterm');
}

/**
 * Screen session information parsed from `screen -ls` output
 */
export interface ScreenSession {
  /** Process ID of the screen session */
  pid: number;
  /** Session name (e.g., "project-windowId") */
  name: string;
  /** Whether the session is currently attached */
  attached: boolean;
  /** Full session identifier (pid.name) */
  fullName: string;
}

/**
 * Parses the output of `screen -ls` command
 *
 * Example output:
 * There are screens on:
 *     12345.project-abc123	(Detached)
 *     12346.project-def456	(Attached)
 * 2 Sockets in /var/folders/.../T/.screen.
 */
function parseScreenLsOutput(output: string): Map<string, ScreenSession> {
  const sessions = new Map<string, ScreenSession>();
  const lines = output.split('\n');

  for (const line of lines) {
    // Match lines like: "	12345.session-name	(Detached)" or "(Attached)"
    const match = line.match(/^\s*(\d+)\.([^\s]+)\s+\((Attached|Detached)\)/);
    if (match) {
      const [, pidStr, name, status] = match;
      const pid = parseInt(pidStr, 10);
      const attached = status === 'Attached';
      const fullName = `${pid}.${name}`;

      sessions.set(name, {
        pid,
        name,
        attached,
        fullName,
      });
    }
  }

  return sessions;
}

/**
 * BFS-walks the process table to find all descendant PIDs of a root process.
 * Uses `ps -eo pid,ppid` via execFile (no shell) for safety.
 * @returns Array of descendant PIDs (excludes the root itself)
 */
export async function getDescendantPids(rootPid: number): Promise<number[]> {
  try {
    const { stdout } = await execFileAsync('ps', ['-eo', 'pid,ppid'], { timeout: 5000 });
    const lines = stdout.trim().split('\n').slice(1); // skip header

    // Build parent→children map
    const children = new Map<number, number[]>();
    for (const line of lines) {
      const parts = line.trim().split(/\s+/);
      if (parts.length < 2) continue;
      const pid = parseInt(parts[0], 10);
      const ppid = parseInt(parts[1], 10);
      if (isNaN(pid) || isNaN(ppid)) continue;
      if (!children.has(ppid)) children.set(ppid, []);
      children.get(ppid)!.push(pid);
    }

    // BFS from root
    const descendants: number[] = [];
    const queue = [rootPid];
    while (queue.length > 0) {
      const current = queue.shift()!;
      const kids = children.get(current) || [];
      for (const kid of kids) {
        descendants.push(kid);
        queue.push(kid);
      }
    }

    return descendants;
  } catch (error) {
    logger.warn('Failed to get descendant PIDs:', error);
    return [];
  }
}

/**
 * BFS-walks the process tree to find the first descendant whose command name
 * matches "claude" (case-insensitive). Used to locate Claude's PID inside
 * a daemon's process tree for MCP gateway cleanup.
 *
 * Uses `ps -eo pid,ppid,comm` — includes the command name column.
 * @returns Claude's PID, or null if not found
 */
export async function findClaudePidInTree(rootPid: number): Promise<number | null> {
  try {
    const { stdout } = await execFileAsync('ps', ['-eo', 'pid,ppid,comm'], { timeout: 5000 });
    const lines = stdout.trim().split('\n').slice(1); // skip header

    // Build parent→children map + command lookup
    const children = new Map<number, number[]>();
    const commands = new Map<number, string>();
    for (const line of lines) {
      const parts = line.trim().split(/\s+/);
      if (parts.length < 3) continue;
      const pid = parseInt(parts[0], 10);
      const ppid = parseInt(parts[1], 10);
      const comm = parts.slice(2).join(' ');
      if (isNaN(pid) || isNaN(ppid)) continue;
      if (!children.has(ppid)) children.set(ppid, []);
      children.get(ppid)!.push(pid);
      commands.set(pid, comm);
    }

    // BFS from root — return first "claude" descendant
    const queue = [rootPid];
    while (queue.length > 0) {
      const current = queue.shift()!;
      const kids = children.get(current) || [];
      for (const kid of kids) {
        const comm = (commands.get(kid) || '').toLowerCase();
        if (comm.includes('claude')) {
          return kid;
        }
        queue.push(kid);
      }
    }

    return null;
  } catch (error) {
    logger.warn('Failed to find Claude PID in tree:', error);
    return null;
  }
}

/**
 * Sends SIGTERM to all PIDs, waits 2s, then SIGKILL to any survivors.
 * Fast path: returns immediately if pids array is empty (no 2s penalty).
 */
export async function killDescendants(pids: number[]): Promise<void> {
  if (pids.length === 0) return;

  // SIGTERM all
  for (const pid of pids) {
    auditedKill(pid, 'SIGTERM', 'killDescendants: SIGTERM descendant');
  }

  // Wait 2s for graceful shutdown
  await new Promise(resolve => setTimeout(resolve, 2000));

  // SIGKILL survivors
  let killed = 0;
  for (const pid of pids) {
    if (auditedKill(pid, 'SIGKILL', 'killDescendants: SIGKILL survivor')) {
      killed++;
    }
  }

  if (killed > 0) {
    logger.info(`SIGKILL sent to ${killed} surviving descendant(s)`);
  }
}

/**
 * Screen CLI wrapper for ImmorTerm extension
 * Provides methods for interacting with GNU Screen sessions
 */
export const screenCommands = {
  /**
   * Lists all active Screen sessions
   * @returns Map of session name to session info
   */
  async listSessions(): Promise<Map<string, ScreenSession>> {
    try {
      const screen = getScreenBinary();
      const { stdout } = await execFileAsync(screen, ['-ls'], { timeout: 5000, killSignal: 'SIGKILL' });
      return parseScreenLsOutput(stdout);
    } catch (error: unknown) {
      // screen -ls returns exit code 1 when there are sessions (or no sessions)
      // but still puts output on stdout — execFileAsync puts stdout on the error object
      if (error && typeof error === 'object' && 'stdout' in error) {
        const stdout = (error as { stdout: string }).stdout;
        if (stdout.includes('No Sockets found') || stdout.includes('No screens found')) {
          return new Map();
        }
        // There might be sessions even with error exit code
        return parseScreenLsOutput(stdout);
      }
      logger.warn('Failed to list screen sessions:', error);
      return new Map();
    }
  },

  /**
   * Kills a Screen session by name, including all descendant processes.
   * 1. Looks up the screen PID via listSessions()
   * 2. If found: BFS-walks the process tree, SIGTERM → 2s → SIGKILL
   * 3. Sends screen -X quit to tear down the session
   * @param sessionName The name of the session to kill (without pid prefix)
   * @returns true if the session was killed, false otherwise
   */
  async killSession(sessionName: string): Promise<boolean> {
    try {
      // Step 1: Look up screen PID to kill descendant processes
      const sessions = await this.listSessions();
      const session = sessions.get(sessionName);
      if (session) {
        const pids = await getDescendantPids(session.pid);
        if (pids.length > 0) {
          logger.info(`Killing ${pids.length} descendant(s) of screen session ${sessionName} (pid ${session.pid})`);
        }
        await killDescendants(pids);
      }

      // Step 2: Quit the screen session itself
      const screen = getScreenBinary();
      await execAsync(`${screen} -S "${sessionName}" -X quit`);
      logger.debug(`Killed screen session: ${sessionName}`);
      return true;
    } catch (error) {
      logger.warn(`Failed to kill screen session ${sessionName}:`, error);
      return false;
    }
  },

  /**
   * Checks if a Screen session exists
   * @param sessionName The name of the session to check
   * @returns true if the session exists, false otherwise
   */
  async sessionExists(sessionName: string): Promise<boolean> {
    const sessions = await this.listSessions();
    return sessions.has(sessionName);
  },

  /**
   * Checks if GNU Screen is installed and available
   * @returns true if screen is installed, false otherwise
   */
  async isScreenInstalled(): Promise<boolean> {
    try {
      const screen = getScreenBinary();
      await execAsync(`which ${screen}`);
      return true;
    } catch {
      return false;
    }
  },

  /**
   * Sync version — no event loop yield, safe for activation in high-RSS environments.
   */
  isScreenInstalledSync(): boolean {
    try {
      const screen = getScreenBinary();
      execFileSync('which', [screen], { stdio: 'ignore' });
      return true;
    } catch {
      return false;
    }
  },

  /**
   * Gets the full path to the screen executable
   * @returns Path to screen, or null if not found
   */
  async getScreenPath(): Promise<string | null> {
    try {
      const screen = getScreenBinary();
      const { stdout } = await execAsync(`which ${screen}`);
      return stdout.trim();
    } catch {
      return null;
    }
  },

  /**
   * Sync version — no event loop yield.
   */
  getScreenPathSync(): string | null {
    try {
      const screen = getScreenBinary();
      return execFileSync('which', [screen], { encoding: 'utf-8' }).trim();
    } catch {
      return null;
    }
  },

  /**
   * Sends a command to a screen session
   * @param sessionName The session to send the command to
   * @param command The command to send
   */
  async sendCommand(sessionName: string, command: string): Promise<boolean> {
    try {
      const screen = getScreenBinary();
      await execAsync(`${screen} -S "${sessionName}" -X stuff "${command}"`);
      return true;
    } catch (error) {
      logger.warn(`Failed to send command to ${sessionName}:`, error);
      return false;
    }
  },

  /**
   * Detaches from a screen session (if attached elsewhere)
   * @param sessionName The session to detach from
   */
  async detachSession(sessionName: string): Promise<boolean> {
    try {
      const screen = getScreenBinary();
      await execAsync(`${screen} -S "${sessionName}" -d`);
      return true;
    } catch {
      return false;
    }
  },

  /**
   * Lists all sessions matching a project pattern
   * @param projectName The project name prefix to match
   * @returns Array of matching session info
   */
  async listProjectSessions(projectName: string): Promise<ScreenSession[]> {
    const sessions = await this.listSessions();
    const matching: ScreenSession[] = [];

    for (const [name, session] of sessions) {
      if (name.startsWith(`${projectName}-`)) {
        matching.push(session);
      }
    }

    return matching;
  },

  /**
   * Kills all sessions matching a project pattern
   * @param projectName The project name prefix to match
   * @returns Number of sessions killed
   */
  async killProjectSessions(projectName: string): Promise<number> {
    const sessions = await this.listProjectSessions(projectName);
    let killed = 0;

    for (const session of sessions) {
      if (await this.killSession(session.name)) {
        killed++;
      }
    }

    logger.info(`Killed ${killed} screen sessions for project: ${projectName}`);
    return killed;
  },

  /**
   * Gets the current window title of a screen session
   * Uses `screen -Q title` to query the title (requires screen 4.1.0+)
   * @param sessionName The session to query
   * @returns The window title, or null if not available
   */
  async getWindowTitle(sessionName: string): Promise<string | null> {
    try {
      const screen = getScreenBinary();
      const { stdout } = await execAsync(`${screen} -S "${sessionName}" -Q title`);
      const title = stdout.trim();
      return title || null;
    } catch {
      // screen -Q may not be available on older versions
      return null;
    }
  },

  /**
   * Sets the window title of a screen session
   * @param sessionName The session to update
   * @param title The new title
   */
  async setWindowTitle(sessionName: string, title: string): Promise<boolean> {
    try {
      const screen = getScreenBinary();
      // Update screen's internal window title
      await execAsync(`${screen} -S "${sessionName}" -X title "${title}"`);

      // NOTE: We don't use "screen -X stuff" for OSC sequences
      // That command injects into terminal INPUT, not output - it would type garbage
      // into whatever program is running (Claude, vim, etc.)

      logger.debug(`Set screen title for ${sessionName}: "${title}"`);
      return true;
    } catch (error) {
      logger.warn(`Failed to set screen title for ${sessionName}:`, error);
      return false;
    }
  },

  /**
   * Sets an environment variable in a screen session's internal environment
   * This is used for IPC between TypeScript and shell scripts via `screen -Q echo`
   * @param sessionName The session to update
   * @param varName The environment variable name
   * @param value The value to set (empty string to clear)
   */
  async setEnv(sessionName: string, varName: string, value: string): Promise<boolean> {
    try {
      const screen = getScreenBinary();
      await execAsync(`${screen} -S "${sessionName}" -X setenv ${varName} "${value}"`);
      logger.debug(`Set env ${varName}="${value}" on session ${sessionName}`);
      return true;
    } catch (error) {
      logger.warn(`Failed to set env ${varName} for ${sessionName}:`, error);
      return false;
    }
  },

  /**
   * Queries a screen session environment variable via `screen -Q echo`.
   * Returns the value or null if not set / query fails.
   * @param sessionName The session to query
   * @param varName The environment variable name
   */
  async getEnv(sessionName: string, varName: string): Promise<string | null> {
    try {
      const screen = getScreenBinary();
      const { stdout } = await execAsync(`${screen} -S "${sessionName}" -Q echo '$${varName}'`);
      const val = stdout.trim();
      // screen -Q returns the literal string '$VAR' if not set
      if (!val || val === `$${varName}`) {
        return null;
      }
      return val;
    } catch {
      return null;
    }
  },

  /**
   * Gets the configured screen binary name
   * @returns The screen binary path
   */
  getScreenBinary,
};

export default screenCommands;
