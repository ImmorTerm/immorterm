/**
 * TaskStorage — Atomic JSON persistence for tasks.
 *
 * File: ~/.immorterm/tasks/<projectId>.json
 * Supports external writes (MCP tools in the daemon) via fs.watch().
 */

import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import * as crypto from 'crypto';
import { EventEmitter } from 'events';
import type { Task, TaskFile, TaskType, TaskLane, TaskStatus, TaskContext } from './types';

const TASKS_DIR = path.join(os.homedir(), '.immorterm', 'tasks');

export class TaskStorage extends EventEmitter {
  private filePath: string;
  private tasks: Task[] = [];
  private watcher: fs.FSWatcher | null = null;
  private writeInProgress = false;

  constructor(projectId: string) {
    super();
    this.filePath = path.join(TASKS_DIR, `${projectId}.json`);
    this.ensureDir();
    this.load();
    this.watchFile();
  }

  // ── CRUD ────────────────────────────────────────────────────

  create(title: string, type: TaskType = 'other', lane: TaskLane = 'next', context?: TaskContext, description?: string): Task {
    const now = Date.now();
    const task: Task = {
      id: crypto.randomUUID(),
      title,
      description,
      type,
      lane,
      status: 'todo',
      createdAt: now,
      updatedAt: now,
      context,
      linkedSessions: [],
    };
    this.tasks.push(task);
    this.save();
    this.emit('change');
    return task;
  }

  update(taskId: string, fields: Partial<Pick<Task, 'title' | 'description' | 'type' | 'lane' | 'status' | 'context'>>): Task | null {
    const task = this.tasks.find(t => t.id === taskId);
    if (!task) return null;

    if (fields.title !== undefined) task.title = fields.title;
    if (fields.description !== undefined) task.description = fields.description;
    if (fields.type !== undefined) task.type = fields.type;
    if (fields.lane !== undefined) task.lane = fields.lane;
    if (fields.context !== undefined) task.context = fields.context;
    if (fields.status !== undefined) {
      task.status = fields.status;
      if (fields.status === 'done' && !task.completedAt) {
        task.completedAt = Date.now();
      }
    }
    task.updatedAt = Date.now();

    this.save();
    this.emit('change');
    return task;
  }

  delete(taskId: string): boolean {
    const idx = this.tasks.findIndex(t => t.id === taskId);
    if (idx === -1) return false;
    this.tasks.splice(idx, 1);
    this.save();
    this.emit('change');
    return true;
  }

  getById(taskId: string): Task | undefined {
    return this.tasks.find(t => t.id === taskId);
  }

  list(filters?: { lane?: TaskLane; status?: TaskStatus; linkedTo?: string }): Task[] {
    let result = [...this.tasks];
    if (filters?.lane) result = result.filter(t => t.lane === filters.lane);
    if (filters?.status) result = result.filter(t => t.status === filters.status);
    if (filters?.linkedTo) {
      const wid = filters.linkedTo;
      result = result.filter(t => t.linkedSessions.some(s => s.immortermId === wid));
    }
    return result;
  }

  /** Get all tasks grouped by lane, sorted by updatedAt desc within each lane. */
  getByLane(): Record<TaskLane, Task[]> {
    const lanes: Record<TaskLane, Task[]> = { now: [], next: [], later: [] };
    for (const task of this.tasks) {
      lanes[task.lane].push(task);
    }
    // Sort: done tasks sink to bottom, then by updatedAt desc
    for (const lane of Object.values(lanes)) {
      lane.sort((a, b) => {
        if (a.status === 'done' && b.status !== 'done') return 1;
        if (a.status !== 'done' && b.status === 'done') return -1;
        return b.updatedAt - a.updatedAt;
      });
    }
    return lanes;
  }

  // ── Session linkage ─────────────────────────────────────────

  linkSession(taskId: string, immortermId: string, sessionName: string): Task | null {
    const task = this.tasks.find(t => t.id === taskId);
    if (!task) return null;

    // Don't duplicate
    if (task.linkedSessions.some(s => s.immortermId === immortermId)) return task;

    task.linkedSessions.push({ immortermId, sessionName, linkedAt: Date.now() });
    if (task.status === 'todo') task.status = 'in_progress';
    if (task.lane !== 'now') task.lane = 'now';
    task.updatedAt = Date.now();

    this.save();
    this.emit('change');
    return task;
  }

  unlinkSession(taskId: string, immortermId: string): Task | null {
    const task = this.tasks.find(t => t.id === taskId);
    if (!task) return null;

    task.linkedSessions = task.linkedSessions.filter(s => s.immortermId !== immortermId);
    task.updatedAt = Date.now();

    this.save();
    this.emit('change');
    return task;
  }

  /** Count tasks linked to a session that are not done. */
  countActiveForSession(immortermId: string): number {
    return this.tasks.filter(
      t => t.status !== 'done' && t.linkedSessions.some(s => s.immortermId === immortermId),
    ).length;
  }

  /** Get in-progress tasks linked to a session (for completion prompts). */
  getInProgressForSession(immortermId: string): Task[] {
    return this.tasks.filter(
      t => t.status === 'in_progress' && t.linkedSessions.some(s => s.immortermId === immortermId),
    );
  }

  // ── Reorder ─────────────────────────────────────────────────

  reorder(taskIds: string[]): void {
    const taskMap = new Map(this.tasks.map(t => [t.id, t]));
    const reordered: Task[] = [];
    for (const id of taskIds) {
      const task = taskMap.get(id);
      if (task) reordered.push(task);
    }
    // Append any tasks not in the new order (shouldn't happen, safety net)
    for (const task of this.tasks) {
      if (!taskIds.includes(task.id)) reordered.push(task);
    }
    this.tasks = reordered;
    this.save();
    this.emit('change');
  }

  // ── Persistence ─────────────────────────────────────────────

  private ensureDir(): void {
    try { fs.mkdirSync(TASKS_DIR, { recursive: true }); } catch { /* exists */ }
  }

  private load(): void {
    try {
      const raw = fs.readFileSync(this.filePath, 'utf-8');
      const data: TaskFile = JSON.parse(raw);
      if (data.version === 1 && Array.isArray(data.tasks)) {
        this.tasks = data.tasks;
      }
    } catch {
      // File doesn't exist or is corrupt — start empty
      this.tasks = [];
    }
  }

  /** Atomic write: write to .tmp, then rename. */
  private save(): void {
    this.writeInProgress = true;
    const data: TaskFile = { version: 1, tasks: this.tasks };
    const tmpPath = this.filePath + '.tmp';
    try {
      fs.writeFileSync(tmpPath, JSON.stringify(data, null, 2));
      fs.renameSync(tmpPath, this.filePath);
    } catch (err) {
      // Best effort — log but don't crash
      console.error('TaskStorage: failed to save', err);
    }
    // Small delay before clearing flag so watcher ignores our own write
    setTimeout(() => { this.writeInProgress = false; }, 100);
  }

  /** Watch for external changes (MCP tool writes from daemon). */
  private watchFile(): void {
    try {
      this.watcher = fs.watch(path.dirname(this.filePath), (_eventType, filename) => {
        if (filename === path.basename(this.filePath) && !this.writeInProgress) {
          this.load();
          this.emit('change');
          this.emit('external-change');
        }
      });
    } catch {
      // Watch not supported — tasks still work, just no live sync from MCP
    }
  }

  /** Get a serializable snapshot for sending to the webview. */
  toJSON(): TaskFile {
    return { version: 1, tasks: [...this.tasks] };
  }

  dispose(): void {
    this.watcher?.close();
    this.watcher = null;
    this.removeAllListeners();
  }
}
