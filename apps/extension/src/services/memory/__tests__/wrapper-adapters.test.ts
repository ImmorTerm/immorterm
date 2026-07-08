/**
 * Phase A T3/T4 — wrapper adapter tests.
 *
 * Each wrapper sits at `${project}/.immorterm/hooks/lib/<name>.sh` after install.
 * These tests render the wrapper bodies to a temp dir alongside dummy upstream
 * hook scripts that simply echo the JSON they receive on stdin, then pipe a
 * sample vendor envelope through `bash` and assert the re-keyed JSON shape.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import { mkdtempSync, rmSync, mkdirSync } from 'fs';
import { tmpdir } from 'os';
import { execFileSync } from 'child_process';

import {
  CURSOR_ADAPTER_SH,
  WINDSURF_ADAPTER_SH,
  CLINE_ADAPTER_SH,
  AIDER_POST_COMMIT_SH,
  writeAllVendorConfigs,
} from '../hook-installer';
import {
  defaultVendorsConfig,
  writeProjectConfig,
  type ProjectConfig,
} from '../../../utils/immorterm-config';

/** Names of upstream hook scripts the wrappers dispatch to. We stub each one
 *  with a tiny script that just records its name and pipes stdin to a logfile.
 */
const UPSTREAM_HOOKS = [
  'immorterm-code-change-capture.sh',
  'immorterm-memory-guide.sh',
  'immorterm-user-prompt.sh',
  'immorterm-plan-sweep.sh',
  'immorterm-pre-compact.sh',
  'immorterm-memory-digest.sh',
];

/** Set up a temp project with `.immorterm/hooks/lib/<wrapper>` and stub upstream hooks. */
function setupProject(): { tmp: string; hooksDir: string; libDir: string; logFile: string } {
  const tmp = mkdtempSync(path.join(tmpdir(), 'immorterm-wrapper-test-'));
  const hooksDir = path.join(tmp, '.immorterm', 'hooks');
  const libDir = path.join(hooksDir, 'lib');
  mkdirSync(libDir, { recursive: true });

  const logFile = path.join(tmp, 'upstream.log');

  // Stub each upstream hook: append "<hookname>\n<stdin>\n---\n" to the logfile.
  for (const hook of UPSTREAM_HOOKS) {
    const p = path.join(hooksDir, hook);
    const body =
      `#!/bin/bash\n` +
      `STDIN="$(cat)"\n` +
      `{ echo "${hook}"; echo "$STDIN"; echo "---"; } >> "${logFile}"\n` +
      `# Pass through stdin as JSON for cline-adapter passthrough test\n` +
      `printf '%s' "$STDIN"\n`;
    fs.writeFileSync(p, body, { mode: 0o755 });
  }

  return { tmp, hooksDir, libDir, logFile };
}

function writeWrapper(libDir: string, name: string, content: string): string {
  const p = path.join(libDir, name);
  fs.writeFileSync(p, content, { mode: 0o755 });
  return p;
}

function pipeToBash(scriptPath: string, stdin: string, args: string[] = []): string {
  return execFileSync('bash', [scriptPath, ...args], {
    input: stdin,
    encoding: 'utf8',
  }).toString();
}

function readLog(logFile: string): string {
  if (!fs.existsSync(logFile)) return '';
  return fs.readFileSync(logFile, 'utf8');
}

describe('Phase A T3 — cursor-adapter.sh', () => {
  let proj: ReturnType<typeof setupProject>;

  beforeEach(() => {
    proj = setupProject();
  });

  afterEach(() => {
    rmSync(proj.tmp, { recursive: true, force: true });
  });

  it('re-keys afterFileEdit → PostToolUse(Edit) and dispatches to code-change-capture', () => {
    const wrapper = writeWrapper(proj.libDir, 'cursor-adapter.sh', CURSOR_ADAPTER_SH);
    const envelope = JSON.stringify({
      conversation_id: 'cursor-conv-123',
      file_path: '/tmp/foo.txt',
      hook_event_name: 'afterFileEdit',
      cursor_version: '1.7.0',
      edits: [{ old: 'a', new: 'b' }],
    });
    pipeToBash(wrapper, envelope);

    const log = readLog(proj.logFile);
    expect(log).toContain('immorterm-code-change-capture.sh');
    expect(log).toContain('"hook_event_name": "PostToolUse"');
    expect(log).toContain('"tool_name": "Edit"');
    expect(log).toContain('"session_id": "cursor-conv-123"');
    expect(log).toContain('"file_path": "/tmp/foo.txt"');
  });

  it('re-keys beforeShellExecution → PreToolUse(Bash) (no-op upstream)', () => {
    const wrapper = writeWrapper(proj.libDir, 'cursor-adapter.sh', CURSOR_ADAPTER_SH);
    const envelope = JSON.stringify({
      conversation_id: 'c1',
      hook_event_name: 'beforeShellExecution',
      command: 'ls -la',
    });
    // Should exit cleanly even though no upstream PreToolUse hook fires.
    expect(() => pipeToBash(wrapper, envelope)).not.toThrow();
  });

  it('re-keys userPromptSubmit → UserPromptSubmit and dispatches to user-prompt.sh', () => {
    const wrapper = writeWrapper(proj.libDir, 'cursor-adapter.sh', CURSOR_ADAPTER_SH);
    const envelope = JSON.stringify({
      conversation_id: 'c1',
      hook_event_name: 'userPromptSubmit',
      prompt: 'hello',
    });
    pipeToBash(wrapper, envelope);
    const log = readLog(proj.logFile);
    expect(log).toContain('immorterm-user-prompt.sh');
    expect(log).toContain('"prompt": "hello"');
  });

  it('handles agentResponse as no-op (no Claude equivalent)', () => {
    const wrapper = writeWrapper(proj.libDir, 'cursor-adapter.sh', CURSOR_ADAPTER_SH);
    const envelope = JSON.stringify({
      conversation_id: 'c1',
      hook_event_name: 'agentResponse',
    });
    pipeToBash(wrapper, envelope);
    expect(readLog(proj.logFile)).toBe('');
  });

  it('re-keys stop → Stop and dispatches to plan-sweep.sh', () => {
    const wrapper = writeWrapper(proj.libDir, 'cursor-adapter.sh', CURSOR_ADAPTER_SH);
    const envelope = JSON.stringify({
      conversation_id: 'c1',
      hook_event_name: 'stop',
    });
    pipeToBash(wrapper, envelope);
    expect(readLog(proj.logFile)).toContain('immorterm-plan-sweep.sh');
  });
});

describe('Phase A T3 — windsurf-adapter.sh', () => {
  let proj: ReturnType<typeof setupProject>;

  beforeEach(() => {
    proj = setupProject();
  });

  afterEach(() => {
    rmSync(proj.tmp, { recursive: true, force: true });
  });

  it('re-keys post_write_code → PostToolUse(Write) and dispatches to code-change-capture', () => {
    const wrapper = writeWrapper(proj.libDir, 'windsurf-adapter.sh', WINDSURF_ADAPTER_SH);
    const envelope = JSON.stringify({
      agent_action_name: 'post_write_code',
      trajectory_id: 'wind-traj-1',
      tool_info: {
        name: 'Write',
        input: { file_path: '/tmp/foo.txt' },
        is_edit: false,
      },
    });
    pipeToBash(wrapper, envelope);
    const log = readLog(proj.logFile);
    expect(log).toContain('immorterm-code-change-capture.sh');
    expect(log).toContain('"hook_event_name": "PostToolUse"');
    expect(log).toContain('"tool_name": "Write"');
    expect(log).toContain('"session_id": "wind-traj-1"');
  });

  it('post_write_code with is_edit=true → tool_name = Edit', () => {
    const wrapper = writeWrapper(proj.libDir, 'windsurf-adapter.sh', WINDSURF_ADAPTER_SH);
    const envelope = JSON.stringify({
      agent_action_name: 'post_write_code',
      trajectory_id: 'wind-traj-2',
      tool_info: { name: '', input: {}, is_edit: true },
    });
    pipeToBash(wrapper, envelope);
    expect(readLog(proj.logFile)).toContain('"tool_name": "Edit"');
  });

  it('re-keys post_cascade_response → Stop', () => {
    const wrapper = writeWrapper(proj.libDir, 'windsurf-adapter.sh', WINDSURF_ADAPTER_SH);
    const envelope = JSON.stringify({
      agent_action_name: 'post_cascade_response',
      trajectory_id: 'wind-traj-3',
    });
    pipeToBash(wrapper, envelope);
    expect(readLog(proj.logFile)).toContain('immorterm-plan-sweep.sh');
  });

  it('re-keys pre_user_prompt → UserPromptSubmit', () => {
    const wrapper = writeWrapper(proj.libDir, 'windsurf-adapter.sh', WINDSURF_ADAPTER_SH);
    const envelope = JSON.stringify({
      agent_action_name: 'pre_user_prompt',
      trajectory_id: 'wind-traj-4',
      user_prompt: 'do thing',
    });
    pipeToBash(wrapper, envelope);
    const log = readLog(proj.logFile);
    expect(log).toContain('immorterm-user-prompt.sh');
    expect(log).toContain('"prompt": "do thing"');
  });
});

describe('Phase A T3 — cline-adapter.sh', () => {
  let proj: ReturnType<typeof setupProject>;

  beforeEach(() => {
    proj = setupProject();
  });

  afterEach(() => {
    rmSync(proj.tmp, { recursive: true, force: true });
  });

  it('re-keys PostToolUse and emits Cline-shaped JSON response on stdout', () => {
    const wrapper = writeWrapper(proj.libDir, 'cline-adapter.sh', CLINE_ADAPTER_SH);
    const envelope = JSON.stringify({
      taskId: 't1',
      hookName: 'PostToolUse',
      clineVersion: '1.0',
      ts: '1700000000000',
      workspaceRoots: ['/tmp'],
      userId: 'u1',
      model: { provider: 'anthropic', slug: 'sonnet' },
      postToolUse: {
        toolName: 'write_to_file',
        parameters: { file_path: '/tmp/foo.txt' },
        result: 'ok',
        success: true,
        executionTimeMs: 12,
      },
    });
    const stdout = pipeToBash(wrapper, envelope, ['PostToolUse']);

    // Upstream stub echoes its stdin; cline adapter passes valid JSON through.
    // Either the upstream JSON came through verbatim, or fallback {"cancel":false}.
    expect(stdout.trim().length).toBeGreaterThan(0);
    const parsed = JSON.parse(stdout.trim());
    expect(parsed).toBeDefined();

    const log = readLog(proj.logFile);
    expect(log).toContain('immorterm-code-change-capture.sh');
    expect(log).toContain('"tool_name": "write_to_file"');
    expect(log).toContain('"session_id": "t1"');
    expect(log).toContain('"cwd": "/tmp"');
  });

  it('falls back to {"cancel":false} when stdin is empty AND no upstream', () => {
    // Set up a project WITHOUT upstream stubs so the wrapper's fallback path runs.
    const noUpstreamTmp = mkdtempSync(path.join(tmpdir(), 'immorterm-cline-fallback-'));
    const noUpstreamLib = path.join(noUpstreamTmp, '.immorterm', 'hooks', 'lib');
    mkdirSync(noUpstreamLib, { recursive: true });
    const wrapper = writeWrapper(noUpstreamLib, 'cline-adapter.sh', CLINE_ADAPTER_SH);
    try {
      const stdout = pipeToBash(wrapper, '', ['PostToolUse']);
      const parsed = JSON.parse(stdout.trim());
      expect(parsed.cancel).toBe(false);
    } finally {
      rmSync(noUpstreamTmp, { recursive: true, force: true });
    }
  });

  it('re-keys TaskStart → SessionStart', () => {
    const wrapper = writeWrapper(proj.libDir, 'cline-adapter.sh', CLINE_ADAPTER_SH);
    const envelope = JSON.stringify({
      taskId: 't2',
      hookName: 'TaskStart',
      workspaceRoots: ['/tmp'],
    });
    pipeToBash(wrapper, envelope, ['TaskStart']);
    expect(readLog(proj.logFile)).toContain('immorterm-memory-guide.sh');
  });
});

describe('Phase A T4 — aider-post-commit.sh', () => {
  let tmp: string;
  let hooksDir: string;
  let libDir: string;
  let logFile: string;

  beforeEach(() => {
    const proj = setupProject();
    tmp = proj.tmp;
    hooksDir = proj.hooksDir;
    libDir = proj.libDir;
    logFile = proj.logFile;

    // Initialize git in tmp dir so `git rev-parse --show-toplevel` works.
    execFileSync('git', ['init', '-q'], { cwd: tmp });
    execFileSync('git', ['config', 'user.email', 'test@test'], { cwd: tmp });
    execFileSync('git', ['config', 'user.name', 'test'], { cwd: tmp });
    // HOME isolation is per-execFileSync via the env option below — we don't
    // mutate process.env.HOME because that leaks across test files when bun
    // runs them serially.
  });

  afterEach(() => {
    rmSync(tmp, { recursive: true, force: true });
  });

  it('exits silently when no Aider markers present', () => {
    const wrapper = writeWrapper(libDir, 'aider-post-commit.sh', AIDER_POST_COMMIT_SH);
    expect(() => execFileSync('bash', [wrapper], { cwd: tmp, encoding: 'utf8', env: { ...process.env, HOME: tmp } })).not.toThrow();
    expect(readLog(logFile)).toBe('');
  });

  it('synthesizes Stop event and dispatches to digest when chat history advances', () => {
    const wrapper = writeWrapper(libDir, 'aider-post-commit.sh', AIDER_POST_COMMIT_SH);
    // Create Aider markers
    mkdirSync(path.join(tmp, '.aider.tags.cache.v3'), { recursive: true });
    fs.writeFileSync(
      path.join(tmp, '.aider.chat.history.md'),
      '# aider chat started at 2026-04-21\n\n#### user prompt\n\n> assistant reply\n',
      'utf8'
    );

    execFileSync('bash', [wrapper], { cwd: tmp, encoding: 'utf8', env: { ...process.env, HOME: tmp } });

    const log = readLog(logFile);
    expect(log).toContain('immorterm-memory-digest.sh');
    expect(log).toContain('"hook_event_name": "Stop"');
    // macOS /tmp resolves through /private symlink — match the basename.
    const tmpBase = path.basename(tmp);
    expect(log).toContain(`/${tmpBase}"`);

    // Checkpoint file should be written under our isolated HOME.
    const ckptDir = path.join(tmp, '.immorterm', 'aider-checkpoints');
    expect(fs.existsSync(ckptDir)).toBe(true);
    const files = fs.readdirSync(ckptDir);
    expect(files.length).toBeGreaterThanOrEqual(1);
  });

  it('does NOT re-fire when chat history has not advanced since last checkpoint', () => {
    const wrapper = writeWrapper(libDir, 'aider-post-commit.sh', AIDER_POST_COMMIT_SH);
    mkdirSync(path.join(tmp, '.aider.tags.cache.v3'), { recursive: true });
    fs.writeFileSync(
      path.join(tmp, '.aider.chat.history.md'),
      '# aider chat\n#### prompt\n> reply\n',
      'utf8'
    );
    // First run — should fire and update checkpoint.
    execFileSync('bash', [wrapper], { cwd: tmp, encoding: 'utf8', env: { ...process.env, HOME: tmp } });
    fs.writeFileSync(logFile, '', 'utf8'); // clear log

    // Second run — same file, no advance — should not fire.
    execFileSync('bash', [wrapper], { cwd: tmp, encoding: 'utf8', env: { ...process.env, HOME: tmp } });
    expect(readLog(logFile)).toBe('');
  });
});

describe('Phase A T3/T4 — installer materializes real wrapper bodies (not stubs)', () => {
  let tmp: string;

  beforeEach(() => {
    tmp = mkdtempSync(path.join(tmpdir(), 'immorterm-wrapper-install-'));
    mkdirSync(path.join(tmp, '.git'), { recursive: true });
    const cfg: ProjectConfig = {
      version: 3,
      projectId: 'test-proj',
      services: {
        memory: { enabled: false, graph: false },
        mcpGateway: { enabled: false },
        vendors: defaultVendorsConfig(),
      },
    };
    writeProjectConfig(tmp, cfg);
  });

  afterEach(() => {
    rmSync(tmp, { recursive: true, force: true });
  });

  it('writes real wrapper bodies (no /bin/true placeholder)', () => {
    writeAllVendorConfigs(tmp);
    const libDir = path.join(tmp, '.immorterm', 'hooks', 'lib');

    for (const name of ['cursor-adapter.sh', 'windsurf-adapter.sh', 'cline-adapter.sh', 'aider-post-commit.sh']) {
      const body = fs.readFileSync(path.join(libDir, name), 'utf8');
      expect(body).not.toContain('exec /bin/true');
      expect(body).not.toContain('placeholder — populated in Phase A T3/T4');
      expect(body).toContain('# ImmorTerm:');
    }
  });

  it('overwrites old T2 placeholder bodies in place', () => {
    const libDir = path.join(tmp, '.immorterm', 'hooks', 'lib');
    mkdirSync(libDir, { recursive: true });
    // Pretend a previous installer pass wrote the T2 placeholder.
    fs.writeFileSync(
      path.join(libDir, 'cursor-adapter.sh'),
      '#!/bin/bash\nexec /bin/true  # placeholder — populated in Phase A T3/T4\n',
      { mode: 0o755 }
    );

    writeAllVendorConfigs(tmp);

    const after = fs.readFileSync(path.join(libDir, 'cursor-adapter.sh'), 'utf8');
    expect(after).toContain('IMMORTERM_AI_TOOL=cursor');
    expect(after).not.toContain('exec /bin/true');
  });

  it('does NOT clobber a hand-edited wrapper script (no markers)', () => {
    const libDir = path.join(tmp, '.immorterm', 'hooks', 'lib');
    mkdirSync(libDir, { recursive: true });
    const userBody = '#!/bin/bash\n# user customization\necho hi\n';
    fs.writeFileSync(path.join(libDir, 'cursor-adapter.sh'), userBody, { mode: 0o755 });

    writeAllVendorConfigs(tmp);

    expect(fs.readFileSync(path.join(libDir, 'cursor-adapter.sh'), 'utf8')).toBe(userBody);
  });
});
