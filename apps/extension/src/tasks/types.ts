/**
 * Task system types for ImmorTerm AI.
 *
 * Tasks are user-created work items that can be captured quickly,
 * prioritized into lanes, and linked to Claude sessions via drag-and-drop.
 */

export interface Task {
  id: string;
  title: string;
  description?: string; // Markdown-supported description
  type: TaskType;
  lane: TaskLane;
  status: TaskStatus;
  createdAt: number;   // Unix timestamp ms
  updatedAt: number;   // Unix timestamp ms
  completedAt?: number; // Unix timestamp ms

  /** Context captured at creation time. */
  context?: TaskContext;

  /** Sessions this task is linked to (many-to-many). */
  linkedSessions: LinkedSession[];
}

export interface TaskContext {
  cwd?: string;
  selectedText?: string;
  /** OpenMemory session UUID of the session where this task was created. */
  sourceSessionId?: string;
  /** ImmorTerm window ID (e.g. "46122-f83bd45d") of the origin session. */
  sourceImmorTermId?: string;
  /** OpenMemory memory ID of the session summary at creation time (for get_memory_context). */
  sourceMemorySummaryId?: string;
  /** Byte offset into the Claude JSONL transcript pinpointing the moment this task was created. */
  sourceMemoryByteOffset?: number;
  /** Byte length of the captured transcript slice (if known). */
  sourceMemoryByteLength?: number;
  /** Absolute path to the JSONL transcript file the byte offset refers to. */
  sourceMemoryJsonlPath?: string;
}

export interface LinkedSession {
  immortermId: string;
  sessionName: string;
  linkedAt: number;
}

export type TaskType = 'bug' | 'feature' | 'investigate' | 'other';
export type TaskLane = 'now' | 'next' | 'later';
export type TaskStatus = 'todo' | 'in_progress' | 'done';

/** Emoji mapping for task types. */
export const TASK_TYPE_EMOJI: Record<TaskType, string> = {
  bug: '\uD83D\uDC1B',        // 🐛
  feature: '\u2728',           // ✨
  investigate: '\uD83D\uDD0D', // 🔍
  other: '\uD83D\uDCCC',      // 📌
};

export const TASK_TYPE_LABEL: Record<TaskType, string> = {
  bug: 'Bug',
  feature: 'Feature',
  investigate: 'Investigate',
  other: 'Other',
};

export const TASK_LANE_LABEL: Record<TaskLane, string> = {
  now: 'Now',
  next: 'Next',
  later: 'Later',
};

/** Persisted file format. */
export interface TaskFile {
  version: 1;
  tasks: Task[];
}

/** Signal file written to pending-task/ for hook consumption. */
export interface TaskSignal {
  task_id: string;
  task_title: string;
  task_description?: string;
  task_type: TaskType;
  context?: TaskContext;
  linked_sessions: Array<{ immorterm_id: string; session_name: string }>;
  timestamp: number;
}
