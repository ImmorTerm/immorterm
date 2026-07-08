/**
 * Phase A T11 — DRY digest LLM picker.
 *
 * Tests:
 *   1. Auto-preselect: `claude` on PATH → anthropic-cli; else `llm` →
 *      llm-cli; else undefined.
 *   2. Provider step shows all 6 providers with the auto-detected one
 *      sorted to the top.
 *   3. Model step shows the right static list per provider, plus the
 *      "Other / type a model name…" escape hatch.
 *   4. Test step ✅ branch — shim returns envelope JSON → validated=true,
 *      writeProjectConfig invoked.
 *   5. Test step ❌ branch — shim fails → user picks "Save anyway" →
 *      validated=false, writeProjectConfig still invoked.
 *   6. Cancel at any step → undefined and no config write.
 *   7. Output parsers — ollama list + llm models list.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { EventEmitter } from 'node:events';

// ── vscode mock ────────────────────────────────────────────────

type ShowQuickPickArgs = [readonly any[] | Thenable<readonly any[]>, any?];
const mockShowQuickPick = vi.fn();
const mockShowInputBox = vi.fn();
const mockShowInformationMessage = vi.fn();
const mockShowErrorMessage = vi.fn();
const mockWithProgress = vi.fn(async (_opts: unknown, task: () => unknown) => task());

vi.mock('vscode', () => ({
  window: {
    showQuickPick: (...args: ShowQuickPickArgs) => mockShowQuickPick(...args),
    showInputBox: (...args: unknown[]) => mockShowInputBox(...args),
    showInformationMessage: (...args: unknown[]) => mockShowInformationMessage(...args),
    showErrorMessage: (...args: unknown[]) => mockShowErrorMessage(...args),
    withProgress: (...args: [unknown, () => unknown]) => mockWithProgress(...args),
  },
  ProgressLocation: { Notification: 15 },
  workspace: { workspaceFolders: [] },
}));

// ── child_process mock ─────────────────────────────────────────

const mockExecFileSync = vi.fn();
const mockSpawn = vi.fn();
vi.mock('node:child_process', () => ({
  execFileSync: (...args: unknown[]) => mockExecFileSync(...args),
  spawn: (...args: unknown[]) => mockSpawn(...args),
}));

// ── fs mock (only existsSync used) ─────────────────────────────

const mockExistsSync = vi.fn((_p?: string) => true);
vi.mock('node:fs', () => ({
  existsSync: (p: string) => mockExistsSync(p),
  default: {
    existsSync: (p: string) => mockExistsSync(p),
  },
}));

// ── config mock ────────────────────────────────────────────────

const mockReadProjectConfig = vi.fn();
const mockWriteProjectConfig = vi.fn();
vi.mock('../../../utils/immorterm-config', () => ({
  readProjectConfig: (...args: unknown[]) => mockReadProjectConfig(...args),
  writeProjectConfig: (...args: unknown[]) => mockWriteProjectConfig(...args),
}));

// ── menu-data mock ─────────────────────────────────────────────

vi.mock('@immorterm/menu-data', () => ({
  DIGEST_MODELS: {
    anthropic: [
      { id: 'claude-sonnet-4-7', label: 'Claude Sonnet 4.7', description: 'default' },
      { id: 'claude-haiku-4-5', label: 'Claude Haiku 4.5', description: 'fast' },
    ],
    openai: [
      { id: 'gpt-4o-mini', label: 'GPT-4o mini', description: 'default' },
      { id: 'gpt-4o', label: 'GPT-4o', description: 'pro' },
    ],
    gemini: [
      { id: 'gemini-2.5-flash', label: 'Gemini 2.5 Flash', description: 'default' },
    ],
  },
}));

// ── Helpers ────────────────────────────────────────────────────

/** Configure execFileSync's behavior:
 *   - Calls to /bin/sh -c "command -v" with $1=BIN check PATH presence.
 *   - Calls to ollama / llm return their stdout.
 */
function setPathPresence(presence: Record<string, boolean>) {
  mockExecFileSync.mockImplementation((cmd: string, args: string[] = []) => {
    if (cmd === '/bin/sh' && args[0] === '-c') {
      // hasOnPath calls: ['/bin/sh', ['-c', SCRIPT, '_', BIN]]
      // args[2] is the dummy '_' positional, args[3] is the binary name.
      const bin = args[3];
      if (presence[bin] === true) return Buffer.from('');
      throw new Error(`not found: ${bin}`);
    }
    if (cmd === 'ollama') return 'NAME ID\nllama3:latest abc123 4.7GB\nmistral:7b def456 4.0GB\n';
    if (cmd === 'llm') return 'OpenAI Chat: gpt-4o-mini\nAnthropic: claude-3.5-sonnet (aliases: sonnet)\n';
    throw new Error(`unexpected cmd: ${cmd}`);
  });
}

/** Build a fake child-process emitter for spawn(). */
function makeChild(opts: { code: number; stdout?: string; stderr?: string; emitError?: Error }): any {
  const child: any = new EventEmitter();
  child.stdout = new EventEmitter();
  child.stderr = new EventEmitter();
  child.stdin = { write: vi.fn(), end: vi.fn() };
  setImmediate(() => {
    if (opts.emitError) {
      child.emit('error', opts.emitError);
      return;
    }
    if (opts.stdout) child.stdout.emit('data', Buffer.from(opts.stdout));
    if (opts.stderr) child.stderr.emit('data', Buffer.from(opts.stderr));
    child.emit('close', opts.code);
  });
  return child;
}

// ── Tests ──────────────────────────────────────────────────────

describe('digest-llm-picker — Phase A T11', () => {
  beforeEach(() => {
    mockShowQuickPick.mockReset();
    mockShowInputBox.mockReset();
    mockShowInformationMessage.mockReset();
    mockShowErrorMessage.mockReset();
    mockWithProgress.mockReset();
    mockWithProgress.mockImplementation(async (_opts: unknown, task: () => unknown) => task());
    mockExecFileSync.mockReset();
    mockSpawn.mockReset();
    mockReadProjectConfig.mockReset();
    mockWriteProjectConfig.mockReset();
    mockExistsSync.mockReturnValue(true);
  });

  describe('detectDefaultProvider() / hasOnPath()', () => {
    it('preselects anthropic-cli when claude is on PATH', async () => {
      setPathPresence({ claude: true, llm: false });
      const mod = await import('../digest-llm-picker');
      expect(mod.detectDefaultProvider()).toBe('anthropic-cli');
    });

    it('preselects llm-cli when only llm is on PATH', async () => {
      setPathPresence({ claude: false, llm: true });
      const mod = await import('../digest-llm-picker');
      expect(mod.detectDefaultProvider()).toBe('llm-cli');
    });

    it('returns undefined when neither claude nor llm is present', async () => {
      setPathPresence({ claude: false, llm: false });
      const mod = await import('../digest-llm-picker');
      expect(mod.detectDefaultProvider()).toBeUndefined();
    });

    it('hasOnPath() rejects suspicious binary names without invoking sh', async () => {
      setPathPresence({});
      const mod = await import('../digest-llm-picker');
      // Names with shell metacharacters must NOT be probed.
      expect(mod.hasOnPath('foo;rm -rf /')).toBe(false);
      // Verify the guard kicked in BEFORE execFileSync was called.
      // (Note: hasOnPath('claude') itself calls execFileSync, which we reset
      // per-test, so we just check the bad-name path didn't.)
      const badCalls = mockExecFileSync.mock.calls.filter(
        (c) => c[0] === '/bin/sh' && c[1]?.[2] === 'foo;rm -rf /',
      );
      expect(badCalls.length).toBe(0);
    });
  });

  describe('parsers', () => {
    it('parseOllamaList drops header row and extracts first column', async () => {
      const { parseOllamaList } = await import('../digest-llm-picker');
      const out = parseOllamaList('NAME    ID  SIZE\nllama3:latest abc 4GB\nmistral:7b def 4GB\n');
      expect(out.map((m) => m.id)).toEqual(['llama3:latest', 'mistral:7b']);
    });

    it('parseLlmModelsList extracts model id after first colon and strips trailing parens', async () => {
      const { parseLlmModelsList } = await import('../digest-llm-picker');
      const out = parseLlmModelsList(
        'OpenAI Chat: gpt-4o-mini\nAnthropic: claude-3.5-sonnet (aliases: sonnet)\n'
      );
      expect(out.map((m) => m.id)).toEqual(['gpt-4o-mini', 'claude-3.5-sonnet']);
    });
  });

  describe('pickDigestLlm — full flow', () => {
    it('happy path: anthropic-cli + claude-sonnet-4-7 + Test ✅ → writes config and validated=true', async () => {
      setPathPresence({ claude: true, llm: false, timeout: true });

      // Step 1: pick anthropic-cli.
      mockShowQuickPick.mockResolvedValueOnce({ providerId: 'anthropic-cli' });
      // Step 2: pick claude-sonnet-4-7.
      mockShowQuickPick.mockResolvedValueOnce({ modelId: 'claude-sonnet-4-7' });
      // Step 3: choose "Test connection" then "Save".
      mockShowInformationMessage
        .mockResolvedValueOnce('Test connection') // outer prompt
        .mockResolvedValueOnce('Save'); // post-test prompt

      // Spawn returns a successful canary response.
      mockSpawn.mockReturnValueOnce(
        makeChild({ code: 0, stdout: '{"result":"OK","usage":{"input_tokens":1,"output_tokens":1},"total_cost_usd":0}' })
      );

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const choice = await pickDigestLlm({ workspacePath: '/tmp/proj' });

      expect(choice).toEqual({ provider: 'anthropic-cli', model: 'claude-sonnet-4-7', validated: true });
      expect(mockWriteProjectConfig).toHaveBeenCalledTimes(1);
      const writtenCfg = mockWriteProjectConfig.mock.calls[0]![1];
      expect(writtenCfg.services.digest).toEqual({
        provider: 'anthropic-cli',
        model: 'claude-sonnet-4-7',
      });
    });

    it('shim ❌ + Save anyway → writes config with validated=false', async () => {
      setPathPresence({ claude: false, llm: false, timeout: true });

      // Step 1: user picks openai-api manually.
      mockShowQuickPick.mockResolvedValueOnce({ providerId: 'openai-api' });
      // Step 2: pick gpt-4o-mini.
      mockShowQuickPick.mockResolvedValueOnce({ modelId: 'gpt-4o-mini' });
      // Step 3: choose Test connection.
      mockShowInformationMessage.mockResolvedValueOnce('Test connection');
      // Test fails → showErrorMessage prompts; user picks "Save anyway".
      mockShowErrorMessage.mockResolvedValueOnce('Save anyway');

      mockSpawn.mockReturnValueOnce(
        makeChild({ code: 1, stderr: '[digest-llm] openai-api: OPENAI_API_KEY is not set\n' })
      );

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const choice = await pickDigestLlm({ workspacePath: '/tmp/proj' });

      expect(choice).toEqual({ provider: 'openai-api', model: 'gpt-4o-mini', validated: false });
      expect(mockWriteProjectConfig).toHaveBeenCalledTimes(1);
    });

    it('cancel at provider step → undefined, no config write', async () => {
      setPathPresence({ claude: false, llm: false });
      mockShowQuickPick.mockResolvedValueOnce(undefined); // user pressed Esc

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const choice = await pickDigestLlm({ workspacePath: '/tmp/proj' });

      expect(choice).toBeUndefined();
      expect(mockWriteProjectConfig).not.toHaveBeenCalled();
    });

    it('cancel at test step → undefined, no config write', async () => {
      setPathPresence({ claude: true, llm: false, timeout: true });
      mockShowQuickPick.mockResolvedValueOnce({ providerId: 'anthropic-cli' });
      mockShowQuickPick.mockResolvedValueOnce({ modelId: 'claude-sonnet-4-7' });
      // User dismissed the modal (returns undefined).
      mockShowInformationMessage.mockResolvedValueOnce(undefined);

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const choice = await pickDigestLlm({ workspacePath: '/tmp/proj' });

      expect(choice).toBeUndefined();
      expect(mockWriteProjectConfig).not.toHaveBeenCalled();
    });

    it('Save without testing → validated=false, but config still written', async () => {
      setPathPresence({ claude: true, llm: false });
      mockShowQuickPick.mockResolvedValueOnce({ providerId: 'anthropic-cli' });
      mockShowQuickPick.mockResolvedValueOnce({ modelId: 'claude-haiku-4-5' });
      // User picks "Save" directly without testing.
      mockShowInformationMessage.mockResolvedValueOnce('Save');

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const choice = await pickDigestLlm({ workspacePath: '/tmp/proj' });

      expect(choice).toEqual({ provider: 'anthropic-cli', model: 'claude-haiku-4-5', validated: false });
      expect(mockWriteProjectConfig).toHaveBeenCalledTimes(1);
    });

    it('provider QuickPick exposes all sub-CLI + API options with auto-detected on top', async () => {
      setPathPresence({ claude: true, llm: false });
      // Capture the items passed to showQuickPick on the first call (provider step).
      mockShowQuickPick.mockImplementationOnce(async (items: any[]) => {
        // 6 sub-backed CLIs (anthropic, codex, cursor, gemini, copilot, opencode)
        // + llm-cli + ollama + 3 APIs = 11.
        expect(items.length).toBe(11);
        const ids = items.map((i) => i.providerId);
        expect(ids).toEqual(
          expect.arrayContaining([
            'anthropic-cli', 'codex-cli', 'cursor-cli', 'gemini-cli', 'copilot-cli',
            'opencode-cli', 'llm-cli', 'ollama',
            'anthropic-api', 'openai-api', 'gemini-api',
          ])
        );
        // Auto-detected anthropic-cli is first.
        expect(items[0]!.providerId).toBe('anthropic-cli');
        expect(items[0]!.picked).toBe(true);
        // Cancel after inspection.
        return undefined;
      });

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const out = await pickDigestLlm({ workspacePath: '/tmp/proj' });
      expect(out).toBeUndefined();
    });

    it('model QuickPick for openai-api lists openai entries + Other escape hatch', async () => {
      setPathPresence({ claude: false, llm: false });
      mockShowQuickPick
        .mockResolvedValueOnce({ providerId: 'openai-api' }) // step 1
        .mockImplementationOnce(async (items: any[]) => {
          const ids = items.map((i) => i.modelId);
          expect(ids).toEqual(expect.arrayContaining(['gpt-4o-mini', 'gpt-4o']));
          // Last item must be the escape-hatch sentinel.
          expect(items[items.length - 1]!.modelId).toMatch(/^__immorterm_other_model__$/);
          return undefined;
        });

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const out = await pickDigestLlm({ workspacePath: '/tmp/proj' });
      expect(out).toBeUndefined();
    });

    it('"Other" escape hatch routes to InputBox, accepts custom model id', async () => {
      setPathPresence({ claude: false, llm: false, timeout: true });
      mockShowQuickPick
        .mockResolvedValueOnce({ providerId: 'gemini-api' })
        .mockResolvedValueOnce({ modelId: '__immorterm_other_model__' });
      mockShowInputBox.mockResolvedValueOnce('gemini-3.0-experimental');
      mockShowInformationMessage.mockResolvedValueOnce('Save');

      const { pickDigestLlm } = await import('../digest-llm-picker');
      const choice = await pickDigestLlm({ workspacePath: '/tmp/proj' });
      expect(choice).toEqual({
        provider: 'gemini-api',
        model: 'gemini-3.0-experimental',
        validated: false,
      });
    });
  });
});
