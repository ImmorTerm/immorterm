// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createTasksPanel } from '../../resources/gpu-terminal-tasks.js';

function task(id: string, source: string, linked: string[] = []) {
  return {
    id,
    title: `[work #${id}] Task ${id}`,
    type: 'feature',
    lane: 'now',
    status: 'todo',
    createdAt: Date.now(),
    updatedAt: Date.now(),
    context: { sourceImmorTermId: source },
    linkedSessions: linked.map(immortermId => ({ immortermId, sessionName: immortermId, linkedAt: Date.now() })),
  };
}

describe('Tasks panel — session provenance', () => {
  afterEach(() => { document.body.textContent = ''; });

  it('toggles between all tasks and tasks created by or linked to the active session', () => {
    document.body.innerHTML = `
      <div id="tasks-header"><button id="task-session-filter-btn"></button></div>
      <div id="task-list"></div>`;
    let activeId = 'session-a';
    const panel = createTasksPanel({
      taskListEl: document.getElementById('task-list'),
      tasksHeaderEl: document.getElementById('tasks-header'),
      postMessage: vi.fn(),
      getActiveSessionName: () => activeId,
      getActiveSessionId: () => activeId,
      getSessionDisplayName: (id: string) => id,
    });
    panel.setTasks([
      task('1', 'session-a'),
      task('2', 'session-b'),
      task('3', 'session-b', ['session-a']),
    ]);
    expect(document.querySelectorAll('.task-item')).toHaveLength(3);

    (document.getElementById('task-session-filter-btn') as HTMLButtonElement).click();
    expect(document.querySelectorAll('.task-item')).toHaveLength(2);
    expect(document.getElementById('task-session-filter-btn')?.getAttribute('aria-pressed')).toBe('true');

    activeId = 'session-b';
    panel.render();
    expect(document.querySelectorAll('.task-item')).toHaveLength(2);
  });

  it('shows creator-session provenance and navigates to that session', () => {
    document.body.innerHTML = `
      <div id="tasks-header"><button id="task-session-filter-btn"></button></div>
      <div id="task-list"></div>`;
    const postMessage = vi.fn();
    const panel = createTasksPanel({
      taskListEl: document.getElementById('task-list'),
      tasksHeaderEl: document.getElementById('tasks-header'),
      postMessage,
      getActiveSessionName: () => 'session-a',
      getActiveSessionId: () => 'session-a',
      getSessionDisplayName: (id: string) => id === 'session-a' ? 'Factory' : id,
    });
    panel.setTasks([task('1', 'session-a')]);

    document.querySelector('.task-item')?.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    const source = document.querySelector('.task-modal-source-link') as HTMLButtonElement;
    expect(source.textContent).toBe('Factory');
    expect(document.querySelector('.task-modal-source-id')?.textContent).toBe('session-a');
    source.click();
    expect(postMessage).toHaveBeenCalledWith({
      type: 'switch-to-task-session', immortermId: 'session-a',
    });
  });
});
