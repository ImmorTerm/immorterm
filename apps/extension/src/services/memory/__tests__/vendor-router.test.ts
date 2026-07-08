import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { mkdtempSync, rmSync, mkdirSync } from 'fs';
import { tmpdir } from 'os';

import {
  defaultVendorsConfig,
  writeProjectConfig,
  type ProjectConfig,
} from '../../../utils/immorterm-config';
import { writeAllVendorConfigs } from '../hook-installer';

describe('Phase A T2 — vendor-router (writeAllVendorConfigs)', () => {
  let tmp: string;

  beforeEach(() => {
    tmp = mkdtempSync(path.join(tmpdir(), 'immorterm-vendor-router-'));
    // Pretend the project is a git repo for the Aider post-commit branch.
    mkdirSync(path.join(tmp, '.git'), { recursive: true });
  });

  afterEach(() => {
    rmSync(tmp, { recursive: true, force: true });
  });

  function seedConfig(vendorsOverride: Partial<ReturnType<typeof defaultVendorsConfig>> = {}): void {
    const cfg: ProjectConfig = {
      version: 3,
      projectId: 'test-proj',
      services: {
        memory: { enabled: false, graph: false },
        mcpGateway: { enabled: false },
        vendors: { ...defaultVendorsConfig(), ...vendorsOverride },
      },
    };
    writeProjectConfig(tmp, cfg);
  }

  it('materializes all 8 non-Claude vendor config files when all vendors are enabled', () => {
    seedConfig();

    const written = writeAllVendorConfigs(tmp);

    // Stub wrapper scripts seeded under .immorterm/hooks/lib/
    const libDir = path.join(tmp, '.immorterm', 'hooks', 'lib');
    expect(fs.existsSync(path.join(libDir, 'cursor-adapter.sh'))).toBe(true);
    expect(fs.existsSync(path.join(libDir, 'windsurf-adapter.sh'))).toBe(true);
    expect(fs.existsSync(path.join(libDir, 'cline-adapter.sh'))).toBe(true);
    expect(fs.existsSync(path.join(libDir, 'aider-post-commit.sh'))).toBe(true);

    // Codex
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    // Cursor
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(true);
    // Windsurf
    expect(fs.existsSync(path.join(tmp, '.windsurf', 'hooks.json'))).toBe(true);
    // Cline — per-event executable trampolines
    expect(fs.existsSync(path.join(tmp, '.clinerules', 'hooks', 'TaskStart'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.clinerules', 'hooks', 'PostToolUse'))).toBe(true);
    // Aider — appended to .git/hooks/post-commit
    expect(fs.existsSync(path.join(tmp, '.git', 'hooks', 'post-commit'))).toBe(true);
    // opencode
    expect(fs.existsSync(path.join(tmp, 'opencode.json'))).toBe(true);
    // Copilot — .github/hooks/immorterm.json
    expect(fs.existsSync(path.join(tmp, '.github', 'hooks', 'immorterm.json'))).toBe(true);

    // Returned list should include the JSON configs (sanity check).
    expect(written).toEqual(
      expect.arrayContaining([
        path.join(tmp, '.codex', 'hooks.json'),
        path.join(tmp, '.cursor', 'hooks.json'),
        path.join(tmp, '.windsurf', 'hooks.json'),
        path.join(tmp, 'opencode.json'),
        path.join(tmp, '.github', 'hooks', 'immorterm.json'),
      ])
    );
  });

  it('Copilot config has PascalCase events and Copilot-shape entries', () => {
    seedConfig();
    writeAllVendorConfigs(tmp);

    const copilotPath = path.join(tmp, '.github', 'hooks', 'immorterm.json');
    expect(fs.existsSync(copilotPath)).toBe(true);
    const cfg = JSON.parse(fs.readFileSync(copilotPath, 'utf8'));

    // Schema marker version present
    expect(cfg.version).toBe(1);

    // PascalCase event names — required so Copilot emits the
    // Claude-shape stdin envelope verbatim, letting our existing hook
    // scripts read it without re-keying.
    expect(cfg.hooks.SessionStart).toBeDefined();
    expect(cfg.hooks.Stop).toBeDefined();
    expect(cfg.hooks.PostToolUse).toBeDefined();

    // Each entry uses Copilot's flat shape ({type, bash, timeoutSec}),
    // NOT Claude's nested shape ({hooks: [{type, command}]}). If the
    // installer ever drifts back to Claude shape, Copilot silently
    // ignores the entries and digestion stops working — guard that.
    const sessionStart = cfg.hooks.SessionStart[0];
    expect(sessionStart.type).toBe('command');
    expect(typeof sessionStart.bash).toBe('string');
    expect(sessionStart.bash).toMatch(/immorterm-memory-guide\.sh$/);
    expect(typeof sessionStart.timeoutSec).toBe('number');
    expect(sessionStart).not.toHaveProperty('command'); // Claude shape would set this
    expect(sessionStart).not.toHaveProperty('hooks');   // Claude wraps in `hooks: []`

    // Bash paths point at .immorterm/hooks/ (the vendor-neutral
    // location), so all 9 vendors share the same scripts.
    expect(cfg.hooks.Stop[0].bash).toMatch(/\.immorterm\/hooks\//);
    expect(cfg.hooks.PostToolUse[0].bash).toMatch(/\.immorterm\/hooks\//);

    // ai_tool tagging — IMMORTERM_AI_TOOL=copilot must be exported before
    // the hook script runs so memory-guide.sh tags memories correctly.
    // Without this, Copilot sessions would still appear as ai_tool=
    // claude-code in memory (silent vendor mis-attribution).
    for (const event of ['SessionStart', 'Stop', 'PostToolUse']) {
      const entry = cfg.hooks[event][0];
      expect(entry.bash).toMatch(/IMMORTERM_AI_TOOL=copilot/);
    }
  });

  it('skips Copilot when copilot vendor is disabled', () => {
    seedConfig({ copilot: { enabled: false } });
    writeAllVendorConfigs(tmp);

    expect(fs.existsSync(path.join(tmp, '.github', 'hooks', 'immorterm.json'))).toBe(false);
    // Other vendors unaffected
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(true);
  });

  it('skips vendor config writes when that vendor is disabled', () => {
    seedConfig({ cursor: { enabled: false }, opencode: { enabled: false } });

    writeAllVendorConfigs(tmp);

    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(false);
    expect(fs.existsSync(path.join(tmp, 'opencode.json'))).toBe(false);
    // Other vendors still present
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.windsurf', 'hooks.json'))).toBe(true);
  });

  it('idempotently rewrites our own vendor configs (marker preserved)', () => {
    seedConfig();
    writeAllVendorConfigs(tmp);
    const cursorPath = path.join(tmp, '.cursor', 'hooks.json');
    const first = fs.readFileSync(cursorPath, 'utf8');
    expect(first).toContain('_immortermManaged');

    // Second pass should not throw and should still contain the marker.
    expect(() => writeAllVendorConfigs(tmp)).not.toThrow();
    const second = fs.readFileSync(cursorPath, 'utf8');
    expect(second).toContain('_immortermManaged');
  });

  it('does NOT clobber an existing user-owned vendor config (no marker)', () => {
    seedConfig();
    const cursorPath = path.join(tmp, '.cursor', 'hooks.json');
    mkdirSync(path.dirname(cursorPath), { recursive: true });
    const userContent = JSON.stringify({ hooks: { afterFileEdit: ['user-script.sh'] } }, null, 2);
    fs.writeFileSync(cursorPath, userContent, 'utf8');

    writeAllVendorConfigs(tmp);

    const after = fs.readFileSync(cursorPath, 'utf8');
    expect(after).toBe(userContent);
    expect(after).not.toContain('_immortermManaged');
  });

  it('appends Aider block to existing post-commit, idempotently', () => {
    seedConfig({ codex: { enabled: false }, cursor: { enabled: false }, windsurf: { enabled: false }, cline: { enabled: false }, opencode: { enabled: false } });
    const postCommit = path.join(tmp, '.git', 'hooks', 'post-commit');
    mkdirSync(path.dirname(postCommit), { recursive: true });
    fs.writeFileSync(postCommit, '#!/bin/bash\n# user hook content\necho hi\n', { mode: 0o755 });

    writeAllVendorConfigs(tmp);
    const onceContent = fs.readFileSync(postCommit, 'utf8');
    expect(onceContent).toContain('# user hook content');
    expect(onceContent).toContain('# >>> immorterm');
    expect(onceContent).toContain('# <<< immorterm');

    // Second call should not duplicate the block.
    writeAllVendorConfigs(tmp);
    const twiceContent = fs.readFileSync(postCommit, 'utf8');
    const beginCount = (twiceContent.match(/# >>> immorterm/g) || []).length;
    expect(beginCount).toBe(1);
  });

  it('falls back to all-enabled when project config is missing', () => {
    // No config seeded — reader returns null, router uses defaults.
    writeAllVendorConfigs(tmp);
    expect(fs.existsSync(path.join(tmp, '.codex', 'hooks.json'))).toBe(true);
    expect(fs.existsSync(path.join(tmp, '.cursor', 'hooks.json'))).toBe(true);
  });
});
