import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import * as path from 'path';
import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from 'fs';
import { tmpdir } from 'os';

// Mock vscode (registry-client → hub-sidecar → vscode)
vi.mock('vscode', () => ({
  workspace: { workspaceFolders: [] },
  window: {},
}));

import { readProjectId } from '../registry-client';

// Regression guard for the ringtail-org identity split: project.json (canonical,
// daemon-stamped into registry entries) must win over a divergent legacy
// project-id file, or restore skips every session as "wrong project".
describe('readProjectId', () => {
  let tmpDir: string;
  let imDir: string;

  beforeEach(() => {
    tmpDir = mkdtempSync(path.join(tmpdir(), 'immorterm-registry-test-'));
    imDir = path.join(tmpDir, '.immorterm');
    mkdirSync(imDir, { recursive: true });
  });

  afterEach(() => {
    rmSync(tmpDir, { recursive: true, force: true });
  });

  it('prefers project.json id over a divergent legacy project-id', () => {
    writeFileSync(path.join(imDir, 'project.json'), JSON.stringify({ id: 'canonical-uuid', name: 'proj' }));
    writeFileSync(path.join(imDir, 'project-id'), 'divergent-uuid');
    expect(readProjectId(tmpDir)).toBe('canonical-uuid');
  });

  it('falls back to legacy project-id when project.json is missing', () => {
    writeFileSync(path.join(imDir, 'project-id'), 'legacy-uuid\n');
    expect(readProjectId(tmpDir)).toBe('legacy-uuid');
  });

  it('falls back to legacy project-id when project.json is malformed or has empty id', () => {
    writeFileSync(path.join(imDir, 'project.json'), '{not json');
    writeFileSync(path.join(imDir, 'project-id'), 'legacy-uuid');
    expect(readProjectId(tmpDir)).toBe('legacy-uuid');

    writeFileSync(path.join(imDir, 'project.json'), JSON.stringify({ id: '  ', name: 'proj' }));
    expect(readProjectId(tmpDir)).toBe('legacy-uuid');
  });

  it('returns null when neither file exists', () => {
    expect(readProjectId(tmpDir)).toBeNull();
    expect(readProjectId('')).toBeNull();
  });
});
