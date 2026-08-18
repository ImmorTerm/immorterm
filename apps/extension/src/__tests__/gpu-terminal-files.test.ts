// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';
import {
  createFileBrowser,
  fileBrowserApiBase,
  fileBrowserErrorMessage,
  fileBrowserListUrl,
} from '../../resources/gpu-terminal-files.js';

describe('remote Files routing and restore failures', () => {
  it('builds the remote filesystem route when the tab retains remote identity', () => {
    expect(fileBrowserListUrl('http://127.0.0.1:1440', 'docker', '/root/projects/landing'))
      .toBe('http://127.0.0.1:1440/api/v1/remotes/docker/ls?path=%2Froot%2Fprojects%2Flanding');
    expect(fileBrowserApiBase('http://127.0.0.1:1440', 'docker', 'index'))
      .toBe('http://127.0.0.1:1440/api/v1/remotes/docker/files/index');
  });

  it('never exposes a raw host OS error for remote, local, or ambiguous tabs', () => {
    const raw = 'read_dir: No such file or directory (os error 2)';
    const states = [
      fileBrowserErrorMessage({ remoteName: 'docker' }),
      fileBrowserErrorMessage({}),
      fileBrowserErrorMessage({ remoteRestoreState: 'ambiguous', remoteCandidates: ['docker', 'prod'] }),
    ];
    for (const message of states) {
      expect(message).not.toContain(raw);
      expect(message).not.toMatch(/read_dir|os error|ENOENT/i);
    }
    expect(states[0]).toContain('docker');
    expect(states[2]).toContain('docker, prod');
    expect(states[2]).toContain('Remote project picker');
  });

  it('blocks local listing for an unresolved legacy remote and shows the picker action', async () => {
    const treeEl = document.createElement('div');
    const searchInput = document.createElement('input');
    const listDir = vi.fn().mockResolvedValue({
      error: 'read_dir: No such file or directory (os error 2)', entries: [],
    });
    createFileBrowser({
      treeEl, searchInput, getRoot: () => '/root/projects/landing',
      remoteName: '', remoteRestoreState: 'unresolved', remoteCandidates: [],
      hubBaseUrl: 'http://127.0.0.1:1440', listDir,
      openPreview: vi.fn(), onDragState: vi.fn(), onDropOnTerminal: vi.fn(),
      onPastePath: vi.fn(), onOpenInEditor: null, onRevealInFinder: null,
      onOpenTerminalHere: vi.fn(), onCopyPath: vi.fn(),
    });
    await Promise.resolve();
    expect(listDir).not.toHaveBeenCalled();
    expect(treeEl.textContent).toContain('Remote project picker');
    expect(treeEl.textContent).not.toMatch(/read_dir|os error/i);
  });
});
