import { execFileSync } from 'child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';
import { afterEach, describe, expect, it } from 'vitest';
import {
  getLegacyTaskProjectIds,
  getStableProjectId,
} from '../services/memory/project-identity';
import { TaskStorage } from '../tasks/storage';
import type { Task } from '../tasks/types';

const roots: string[] = [];

function tempRoot(label: string): string {
  const root = mkdtempSync(join(tmpdir(), `immorterm-${label}-`));
  roots.push(root);
  return root;
}

function task(id: string, title: string): Task {
  return {
    id,
    title,
    type: 'other',
    lane: 'next',
    status: 'todo',
    createdAt: 1,
    updatedAt: 1,
    linkedSessions: [],
  };
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe('canonical task project identity', () => {
  it('prefers the saved project slug and exposes the former remote id for migration', () => {
    const workspace = tempRoot('project-identity');
    execFileSync('git', ['init', '-q'], { cwd: workspace });
    execFileSync(
      'git',
      ['remote', 'add', 'origin', 'git@github.com:FLAM-Fashion/flam.git'],
      { cwd: workspace },
    );
    mkdirSync(join(workspace, '.claude'));
    writeFileSync(join(workspace, '.claude', 'project-id'), 'flam\n');

    expect(getStableProjectId(workspace)).toBe('flam');
    expect(getLegacyTaskProjectIds(workspace, 'flam')).toEqual(['flam-fashion-flam']);
  });

  it('merges legacy tasks by id without replacing canonical records', () => {
    const tasksDir = tempRoot('task-files');
    writeFileSync(
      join(tasksDir, 'flam.json'),
      JSON.stringify({ version: 1, tasks: [task('same', 'canonical')] }),
    );
    writeFileSync(
      join(tasksDir, 'flam-fashion-flam.json'),
      JSON.stringify({
        version: 1,
        tasks: [task('same', 'legacy'), task('new', 'preserved')],
      }),
    );

    const storage = new TaskStorage('flam', ['flam-fashion-flam'], tasksDir);
    expect(storage.list().map(item => [item.id, item.title])).toEqual([
      ['same', 'canonical'],
      ['new', 'preserved'],
    ]);
    storage.dispose();

    const persisted = JSON.parse(readFileSync(join(tasksDir, 'flam.json'), 'utf8'));
    expect(persisted.tasks).toHaveLength(2);
  });
});
