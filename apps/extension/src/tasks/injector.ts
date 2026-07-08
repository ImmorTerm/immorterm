/**
 * TaskInjector — Writes signal files for hook-based prompt injection.
 *
 * When a task is dropped onto a session, we write a signal file to
 * ~/.immorterm/pending-task/{IMMORTERM_ID}.json. The UserPromptSubmit
 * hook detects this file and injects task context into the prompt.
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import type { Task, TaskSignal } from './types';

const PENDING_TASK_DIR = path.join(os.homedir(), '.immorterm', 'pending-task');

/**
 * Write a task signal file for a target session.
 * Returns a watcher that resolves when the hook consumes the file.
 */
export function writeTaskSignal(
  task: Task,
  targetWindowId: string,
): { signalPath: string } {
  try { fs.mkdirSync(PENDING_TASK_DIR, { recursive: true }); } catch { /* exists */ }

  const signalPath = path.join(PENDING_TASK_DIR, `${targetWindowId}.json`);
  const signal: TaskSignal = {
    task_id: task.id,
    task_title: task.title,
    task_description: task.description,
    task_type: task.type,
    context: task.context,
    linked_sessions: task.linkedSessions.map(s => ({
      immorterm_id: s.immortermId,
      session_name: s.sessionName,
    })),
    timestamp: Date.now(),
  };

  fs.writeFileSync(signalPath, JSON.stringify(signal, null, 2));

  return { signalPath };
}
