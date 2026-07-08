/**
 * Kill Audit Log — persistent file-based log of every process.kill() in the extension.
 *
 * Writes to ~/.immorterm/kill-audit.log so we can diagnose unexpected kills
 * after the fact (VS Code Output panel is lost on crash).
 *
 * Usage:
 *   import { auditedKill } from '../utils/kill-audit';
 *   auditedKill(pid, 'SIGTERM', 'shelved-reaper: expired AI daemon');
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

const AUDIT_LOG_PATH = path.join(os.homedir(), '.immorterm', 'kill-audit.log');
const MAX_LOG_SIZE = 1024 * 1024; // 1 MB — rotate when exceeded

/**
 * Send a signal to a process and log the action to the kill audit file.
 * Returns true if the signal was sent successfully, false if ESRCH/EPERM.
 */
export function auditedKill(
  pid: number,
  signal: NodeJS.Signals | number,
  reason: string,
): boolean {
  const timestamp = new Date().toISOString();
  const line = `${timestamp} | PID ${pid} | ${String(signal).padEnd(7)} | ${reason}\n`;

  // Log to file (best-effort, never throw)
  try {
    // Rotate if too large
    try {
      const stat = fs.statSync(AUDIT_LOG_PATH);
      if (stat.size > MAX_LOG_SIZE) {
        const rotated = AUDIT_LOG_PATH + '.1';
        try { fs.unlinkSync(rotated); } catch { /* ok */ }
        fs.renameSync(AUDIT_LOG_PATH, rotated);
      }
    } catch { /* file doesn't exist yet, fine */ }

    fs.appendFileSync(AUDIT_LOG_PATH, line);
  } catch { /* best-effort */ }

  // Actually send the signal
  try {
    process.kill(pid, signal);
    return true;
  } catch {
    return false;
  }
}
