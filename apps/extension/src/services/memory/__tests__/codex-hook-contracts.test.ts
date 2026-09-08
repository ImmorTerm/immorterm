import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { execFileSync } from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { mkdtempSync, mkdirSync, rmSync } from 'fs';
import { tmpdir } from 'os';

import {
  defaultVendorsConfig,
  writeProjectConfig,
  type ProjectConfig,
} from '../../../utils/immorterm-config';
import { writeAllVendorConfigs } from '../hook-installer';
import {
  CODEX_HOOK_ADAPTER_SH,
  generatePreCompactHook,
} from '../../../../../../libs/services/src/hook-installer';

describe('Codex hook output contracts', () => {
  let tmp: string;

  beforeEach(() => {
    tmp = mkdtempSync(path.join(tmpdir(), 'immorterm-codex-hooks-'));
    mkdirSync(path.join(tmp, '.git'), { recursive: true });
    const defaults = defaultVendorsConfig();
    const config: ProjectConfig = {
      version: 3,
      projectId: 'codex-hook-test',
      services: {
        memory: { enabled: true, graph: false },
        mcpGateway: { enabled: false },
        vendors: { ...defaults, codex: { enabled: true } },
      },
    };
    writeProjectConfig(tmp, config);
  });

  afterEach(() => {
    rmSync(tmp, { recursive: true, force: true });
  });

  function writeExecutable(file: string, body: string): void {
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, body, { mode: 0o755 });
  }

  async function waitForFile(file: string, timeoutMs = 3_000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    while (!fs.existsSync(file) && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
    expect(fs.existsSync(file)).toBe(true);
  }

  it('routes every managed Codex handler through the contract adapter', () => {
    writeAllVendorConfigs(tmp);

    const adapter = path.join(tmp, '.immorterm', 'hooks', 'lib', 'codex-hook-adapter.sh');
    expect(fs.readFileSync(adapter, 'utf8')).toBe(CODEX_HOOK_ADAPTER_SH);

    const config = JSON.parse(fs.readFileSync(path.join(tmp, '.codex', 'hooks.json'), 'utf8'));
    for (const groups of Object.values(config.hooks) as Array<Array<{ hooks: Array<{ command: string }> }>>) {
      for (const group of groups) {
        for (const handler of group.hooks) {
          expect(handler.command).toContain('codex-hook-adapter.sh');
        }
      }
    }
  });

  it('wraps context and emits JSON objects for quiet lifecycle hooks', () => {
    const adapter = path.join(tmp, '.immorterm', 'hooks', 'lib', 'codex-hook-adapter.sh');
    const contextHook = path.join(tmp, 'context-hook.sh');
    const quietHook = path.join(tmp, 'quiet-hook.sh');
    writeExecutable(adapter, CODEX_HOOK_ADAPTER_SH);
    writeExecutable(contextHook, '#!/bin/bash\nprintf \'memory context\\n\'\n');
    writeExecutable(quietHook, '#!/bin/bash\nprintf \'not valid hook JSON\\n\'\n');

    const context = execFileSync('bash', [adapter, 'SessionStart', 'context', contextHook], {
      cwd: tmp,
      input: JSON.stringify({ hook_event_name: 'SessionStart', session_id: 's1', cwd: tmp }),
      encoding: 'utf8',
    });
    expect(JSON.parse(context)).toEqual({
      hookSpecificOutput: {
        hookEventName: 'SessionStart',
        additionalContext: 'memory context\n',
      },
    });

    const quiet = execFileSync('bash', [adapter, 'PreCompact', 'quiet', quietHook], {
      cwd: tmp,
      input: JSON.stringify({ hook_event_name: 'PreCompact', session_id: 's1', cwd: tmp }),
      encoding: 'utf8',
    });
    expect(JSON.parse(quiet)).toEqual({});
  });

  it('returns valid PostToolUse JSON before a slow side effect finishes', async () => {
    const adapter = path.join(tmp, '.immorterm', 'hooks', 'lib', 'codex-hook-adapter.sh');
    const slowHook = path.join(tmp, 'slow-hook.sh');
    const marker = path.join(tmp, 'deferred-finished');
    writeExecutable(adapter, CODEX_HOOK_ADAPTER_SH);
    writeExecutable(
      slowHook,
      `#!/bin/bash\nsleep 1\nprintf 'finished' > '${marker}'\nprintf 'not valid hook JSON\\n'\n`
    );

    const started = Date.now();
    const output = execFileSync('bash', [adapter, 'PostToolUse', 'defer', slowHook], {
      cwd: tmp,
      input: JSON.stringify({ hook_event_name: 'PostToolUse', session_id: 's1', cwd: tmp }),
      encoding: 'utf8',
    });
    const elapsed = Date.now() - started;

    expect(JSON.parse(output)).toEqual({});
    expect(elapsed).toBeLessThan(750);
    expect(fs.existsSync(marker)).toBe(false);
    await waitForFile(marker);
  });

  it('writes recovery before a slow digest and stays fail-soft with dead memory', () => {
    const project = path.join(tmp, 'project');
    const hooks = path.join(project, '.immorterm', 'hooks');
    const home = path.join(tmp, 'home');
    const sessionId = 'codex-precompact-proof';
    const transcript = path.join(tmp, `${sessionId}.jsonl`);
    const preCompact = path.join(hooks, 'immorterm-pre-compact.sh');
    const digest = path.join(hooks, 'immorterm-memory-digest.sh');
    const digestMarker = path.join(tmp, 'digest-finished');
    mkdirSync(hooks, { recursive: true });
    mkdirSync(home, { recursive: true });
    writeExecutable(preCompact, generatePreCompactHook('codex-hook-test'));
    writeExecutable(
      digest,
      `#!/bin/bash\nsleep 2\nprintf 'finished' > '${digestMarker}'\n`
    );
    fs.writeFileSync(
      transcript,
      `${JSON.stringify({ type: 'user', message: { content: 'Keep this recovery context intact.' } })}\n`
    );

    const started = Date.now();
    execFileSync('bash', [preCompact], {
      cwd: project,
      input: JSON.stringify({
        hook_event_name: 'PreCompact',
        session_id: sessionId,
        transcript_path: transcript,
        cwd: project,
        trigger: 'manual',
      }),
      env: {
        ...process.env,
        HOME: home,
        IMMORTERM_MEMORY_URL: 'http://127.0.0.1:9',
        IMMORTERM_HANDOFF_HTTP_TIMEOUT: '0.1',
      },
      stdio: ['pipe', 'pipe', 'pipe'],
      timeout: 5_000,
    });
    const elapsed = Date.now() - started;

    expect(elapsed).toBeLessThan(1_500);
    const handoff = path.join(home, '.immorterm', 'handoff', `immorterm-handoff-${sessionId}.json`);
    const recovery = JSON.parse(fs.readFileSync(handoff, 'utf8'));
    expect(recovery.user_messages).toContain('Keep this recovery context intact.');
    expect(fs.statSync(handoff).mode & 0o777).toBe(0o600);
    expect(fs.existsSync(digestMarker)).toBe(false);
  });
});
