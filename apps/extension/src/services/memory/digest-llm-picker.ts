/**
 * Digest LLM Picker — Phase A T11
 *
 * DRY three-step QuickPick used by:
 *   1. The first-run wizard (services-picker.ts).
 *   2. The "Digest LLM" menu entry (extension.ts → command
 *      `immorterm.configureDigestLlm`).
 *   3. The standalone CLI (`immorterm config set
 *      services.digest.{provider,model}`) — CLI relies on the
 *      shared dot-notation setter so this module isn't imported
 *      there, but it's the canonical interactive flow for the
 *      VS Code surface.
 *
 * Steps:
 *   1. Provider — 6 hardcoded options + auto-preselect by PATH
 *      detection (`claude` → `anthropic-cli`, else `llm` →
 *      `llm-cli`).
 *   2. Model — provider-scoped list (static for hosted APIs,
 *      dynamic for ollama / llm-cli) plus an "Other / type a
 *      model name…" InputBox escape hatch.
 *   3. Test connection — runs the digest-llm-invoke shim with
 *      the chosen provider+model and a 1-line canary prompt
 *      ("Respond with: OK"). User can Test, Save, or Cancel.
 *
 * Returns `undefined` if the user cancels at any step. On Save,
 * writes `services.digest.{provider, model}` to project config
 * via `writeProjectConfig` and returns the choice.
 */

import * as vscode from 'vscode';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { DIGEST_MODELS } from '@immorterm/menu-data';
import { readProjectConfig, writeProjectConfig, type ProjectConfig } from '../../utils/immorterm-config';

// ── Public types ────────────────────────────────────────────────

export type DigestProvider =
  | 'anthropic-cli'
  | 'codex-cli'
  | 'cursor-cli'
  | 'gemini-cli'
  | 'copilot-cli'
  | 'opencode-cli'
  | 'llm-cli'
  | 'ollama'
  | 'anthropic-api'
  | 'openai-api'
  | 'gemini-api';

export interface DigestLlmChoice {
  provider: DigestProvider;
  model: string;
  /** True only when the Test connection step actually returned ✅. */
  validated: boolean;
}

export interface PickDigestLlmOpts {
  workspacePath: string;
  initialProvider?: DigestProvider;
  initialModel?: string;
}

// ── Provider catalog ────────────────────────────────────────────

interface ProviderDef {
  id: DigestProvider;
  label: string;
  description: string;
  /** Binary name to probe on PATH for auto-detect. */
  detectBin?: string;
  /** When the chosen model can't come from a static menu-data list,
   * how do we obtain candidates? */
  modelSource:
    | 'menu-data-anthropic'
    | 'menu-data-openai'
    | 'menu-data-gemini'
    | 'menu-data-copilot'
    | 'dynamic-ollama'
    | 'dynamic-llm-cli';
}

// Order is intentional: subscription-backed CLIs and local engines come
// first because the user is already paying for them. API providers go last
// because they bill per token on top of any vendor subscription the user
// already has. Never reorder so APIs lead — that double-bills users.
const PROVIDERS: ProviderDef[] = [
  {
    id: 'anthropic-cli',
    label: '$(terminal) Anthropic CLI (claude)',
    description: 'Uses your Claude subscription — `claude` on PATH. Recommended.',
    detectBin: 'claude',
    modelSource: 'menu-data-anthropic',
  },
  {
    id: 'codex-cli',
    label: '$(terminal) OpenAI Codex CLI',
    description: 'Uses your ChatGPT Plus/Pro/Business sub via `codex login`',
    detectBin: 'codex',
    modelSource: 'menu-data-openai',
  },
  {
    id: 'cursor-cli',
    label: '$(terminal) Cursor (cursor-agent)',
    description: 'Uses your Cursor Pro sub — counts against monthly Cursor quota',
    detectBin: 'cursor-agent',
    modelSource: 'menu-data-anthropic',
  },
  {
    id: 'gemini-cli',
    label: '$(terminal) Google Gemini CLI',
    description: 'Uses your Google account / Gemini Advanced — needs prior `gemini` login',
    detectBin: 'gemini',
    modelSource: 'menu-data-gemini',
  },
  {
    id: 'copilot-cli',
    label: '$(terminal) GitHub Copilot CLI',
    description: 'Uses your Copilot Pro/Business/Enterprise sub — counts as premium request',
    detectBin: 'copilot',
    modelSource: 'menu-data-copilot',
  },
  {
    id: 'opencode-cli',
    label: '$(terminal) opencode',
    description: 'Routes to whichever provider opencode has configured',
    detectBin: 'opencode',
    modelSource: 'menu-data-anthropic',
  },
  {
    id: 'llm-cli',
    label: '$(terminal) Simon Willison\'s `llm` CLI',
    description: 'Universal — routes to whichever vendor sub you have configured',
    detectBin: 'llm',
    modelSource: 'dynamic-llm-cli',
  },
  {
    id: 'ollama',
    label: '$(server) Ollama (local)',
    description: 'Free, runs locally on your machine via ollama daemon at :11434',
    detectBin: 'ollama',
    modelSource: 'dynamic-ollama',
  },
  {
    id: 'anthropic-api',
    label: '$(cloud) Anthropic API (pay-per-token)',
    description: 'Direct REST — bills ANTHROPIC_API_KEY on top of any subscription',
    modelSource: 'menu-data-anthropic',
  },
  {
    id: 'openai-api',
    label: '$(cloud) OpenAI API (pay-per-token)',
    description: 'Direct REST — bills OPENAI_API_KEY on top of ChatGPT Plus',
    modelSource: 'menu-data-openai',
  },
  {
    id: 'gemini-api',
    label: '$(cloud) Google Gemini API (pay-per-token)',
    description: 'Direct REST — bills GEMINI_API_KEY on top of Gemini Advanced',
    modelSource: 'menu-data-gemini',
  },
];

// ── PATH detection ──────────────────────────────────────────────

/**
 * Returns true iff `bin` resolves on PATH. Uses execFileSync (no shell)
 * with `command -v` via /bin/sh -c, but only with a known-safe argument
 * list so user input never reaches a shell. We accept the small
 * complication because `command -v` is the most reliable cross-platform
 * "is this on PATH" probe — `which` is missing on minimal images.
 */
export function hasOnPath(bin: string): boolean {
  // Reject anything that smells like an injection attempt — we only ever
  // pass static binary names defined in this module.
  if (!/^[A-Za-z0-9_.\-]+$/.test(bin)) return false;
  try {
    execFileSync('/bin/sh', ['-c', `command -v "$1" >/dev/null 2>&1`, '_', bin], { stdio: 'ignore' });
    return true;
  } catch {
    return false;
  }
}

/** Auto-preselect the first sub-backed CLI we find on PATH. Order
 * matches PROVIDERS — claude wins, then codex, cursor, gemini,
 * copilot, opencode, llm, ollama. API providers don't auto-select
 * (they require explicit env vars and double-bill). */
export function detectDefaultProvider(): DigestProvider | undefined {
  for (const p of PROVIDERS) {
    if (!p.detectBin) continue;
    // Skip ollama unless the daemon is reachable — `ollama` on PATH
    // alone is a weak signal; the daemon may not be running. The
    // shim itself will surface that error if user picks it.
    // We don't probe here — keep auto-detect cheap.
    if (hasOnPath(p.detectBin)) return p.id;
  }
  return undefined;
}

// ── Step 1: provider QuickPick ─────────────────────────────────

interface ProviderQuickPickItem extends vscode.QuickPickItem {
  providerId: DigestProvider;
}

async function pickProvider(initial?: DigestProvider): Promise<DigestProvider | undefined> {
  const auto = initial ?? detectDefaultProvider();
  const items: ProviderQuickPickItem[] = PROVIDERS.map((p) => ({
    providerId: p.id,
    label: p.label,
    description: p.description,
    detail: auto === p.id ? 'Detected on PATH — recommended' : undefined,
    picked: auto === p.id,
  }));

  // Sort the auto-preselected provider to the top so it's the first row.
  items.sort((a, b) => {
    if (a.providerId === auto) return -1;
    if (b.providerId === auto) return 1;
    return 0;
  });

  const picked = await vscode.window.showQuickPick(items, {
    title: 'ImmorTerm — Digest LLM (1/3): Provider',
    placeHolder: auto
      ? `Default: ${auto} (detected on PATH). Press Enter to confirm or pick another.`
      : 'Pick a provider for the digest LLM',
    ignoreFocusOut: true,
  });
  return picked?.providerId;
}

// ── Step 2: model QuickPick ────────────────────────────────────

interface ModelCandidate {
  id: string;
  label: string;
  description?: string;
}

const OTHER_MODEL_SENTINEL = '__immorterm_other_model__';

/** Run a binary with a fixed argv (no shell) and return stdout (trimmed).
 * Returns null on failure. Used only for ollama/llm probes. */
function runCmd(cmd: string, args: string[], timeoutMs = 5000): string | null {
  // Defensive — caller should only pass static command names.
  if (!/^[A-Za-z0-9_.\-]+$/.test(cmd)) return null;
  try {
    const out = execFileSync(cmd, args, {
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: timeoutMs,
      encoding: 'utf-8',
    });
    return out.trim();
  } catch {
    return null;
  }
}

/** Parse `ollama list` output. Header row "NAME ...", first column is the model id. */
export function parseOllamaList(stdout: string): ModelCandidate[] {
  const lines = stdout.split('\n').filter((l) => l.trim().length > 0);
  if (lines.length === 0) return [];
  // Drop header row if it starts with NAME / ID (case-insensitive).
  const start = /^(name|id)\b/i.test(lines[0]!) ? 1 : 0;
  const out: ModelCandidate[] = [];
  for (let i = start; i < lines.length; i++) {
    const cols = lines[i]!.split(/\s+/);
    if (cols.length === 0 || !cols[0]) continue;
    out.push({ id: cols[0]!, label: cols[0]!, description: cols.slice(1).join(' ') || undefined });
  }
  return out;
}

/** Parse `llm models list` output. Each line is roughly:
 *   "OpenAI Chat: gpt-4o-mini" or "Anthropic: claude-3.5-sonnet (aliases: ...)".
 * We extract the part after the FIRST colon as the model id. */
export function parseLlmModelsList(stdout: string): ModelCandidate[] {
  const lines = stdout.split('\n').filter((l) => l.trim().length > 0);
  const out: ModelCandidate[] = [];
  for (const line of lines) {
    const trimmed = line.trim();
    const idx = trimmed.indexOf(':');
    if (idx < 0) {
      // No colon — treat whole line as id.
      out.push({ id: trimmed, label: trimmed });
      continue;
    }
    const head = trimmed.slice(0, idx).trim();
    let rest = trimmed.slice(idx + 1).trim();
    // Strip trailing parenthetical (e.g. "(aliases: ...)").
    rest = rest.replace(/\s*\([^)]*\)\s*$/, '').trim();
    if (!rest) continue;
    out.push({ id: rest, label: rest, description: head });
  }
  return out;
}

function modelsForProvider(provider: DigestProvider): { models: ModelCandidate[]; sourceFailed: boolean } {
  const def = PROVIDERS.find((p) => p.id === provider);
  if (!def) return { models: [], sourceFailed: false };
  switch (def.modelSource) {
    case 'menu-data-anthropic':
      return { models: DIGEST_MODELS.anthropic!.map((m) => ({ id: m.id, label: m.label, description: m.description })), sourceFailed: false };
    case 'menu-data-openai':
      return { models: DIGEST_MODELS.openai!.map((m) => ({ id: m.id, label: m.label, description: m.description })), sourceFailed: false };
    case 'menu-data-gemini':
      return { models: DIGEST_MODELS.gemini!.map((m) => ({ id: m.id, label: m.label, description: m.description })), sourceFailed: false };
    case 'menu-data-copilot':
      return { models: DIGEST_MODELS.copilot!.map((m) => ({ id: m.id, label: m.label, description: m.description })), sourceFailed: false };
    case 'dynamic-ollama': {
      const out = runCmd('ollama', ['list']);
      if (out === null) return { models: [], sourceFailed: true };
      return { models: parseOllamaList(out), sourceFailed: false };
    }
    case 'dynamic-llm-cli': {
      const out = runCmd('llm', ['models', 'list']);
      if (out === null) return { models: [], sourceFailed: true };
      return { models: parseLlmModelsList(out), sourceFailed: false };
    }
  }
}

interface ModelQuickPickItem extends vscode.QuickPickItem {
  modelId: string;
}

async function pickModel(provider: DigestProvider, initialModel?: string): Promise<string | undefined> {
  const { models, sourceFailed } = modelsForProvider(provider);

  // If the dynamic source failed, fall straight through to the input box.
  if (sourceFailed) {
    return promptForCustomModel(provider, initialModel);
  }

  const items: ModelQuickPickItem[] = models.map((m) => ({
    modelId: m.id,
    label: m.label,
    description: m.description,
    picked: initialModel === m.id,
  }));

  // Always offer the escape hatch.
  items.push({
    modelId: OTHER_MODEL_SENTINEL,
    label: '$(edit) Other / type a model name…',
    description: 'Custom model id (e.g. self-hosted endpoint, fine-tune)',
  });

  const picked = await vscode.window.showQuickPick(items, {
    title: `ImmorTerm — Digest LLM (2/3): Model for ${provider}`,
    placeHolder: 'Pick a model',
    ignoreFocusOut: true,
  });

  if (!picked) return undefined;
  if (picked.modelId === OTHER_MODEL_SENTINEL) {
    return promptForCustomModel(provider, initialModel);
  }
  return picked.modelId;
}

async function promptForCustomModel(provider: DigestProvider, initialModel?: string): Promise<string | undefined> {
  const value = await vscode.window.showInputBox({
    title: `ImmorTerm — Digest LLM (2/3): Custom model for ${provider}`,
    prompt: 'Type the exact model id the provider expects',
    value: initialModel ?? '',
    ignoreFocusOut: true,
    validateInput: (v) => (v.trim().length === 0 ? 'Model id cannot be empty' : undefined),
  });
  return value?.trim() || undefined;
}

// ── Step 3: test connection ────────────────────────────────────

const CANARY_PROMPT = 'You are a connection test. Reply with the literal text: OK';
const CANARY_INPUT = 'Respond with: OK';
const TEST_TIMEOUT_SECONDS = 10;

interface TestResult {
  ok: boolean;
  /** Stdout from the shim — we expect the canonical envelope JSON. */
  stdout: string;
  /** Last 200 chars of stderr on failure. */
  stderrTail: string;
  durationMs: number;
}

/** Resolve the digest-llm-invoke.sh shim path. Looks first inside the
 * deployed extension's resources/, falls back to the in-tree path. */
export function resolveShimPath(): string {
  // Walk up from this file: src/services/memory/ → up 3 → extension root.
  // Compiled output lives under out/ at the same depth, so __dirname there
  // is .../out/services/memory; same up-3 lands at the extension root.
  const here = __dirname;
  const candidates = [
    path.resolve(here, '..', '..', '..', 'resources', 'hooks', 'digest-llm-invoke.sh'),
    path.resolve(here, '..', '..', '..', '..', 'resources', 'hooks', 'digest-llm-invoke.sh'),
  ];
  for (const c of candidates) {
    try {
      if (fs.existsSync(c)) return c;
    } catch { /* ignore */ }
  }
  return candidates[0]!;
}

/** Pick a portable 10s wall-time wrapper. Returns the argv prefix to prepend. */
function pickTimeoutPrefix(): string[] {
  if (hasOnPath('timeout')) return ['timeout', String(TEST_TIMEOUT_SECONDS)];
  if (hasOnPath('gtimeout')) return ['gtimeout', String(TEST_TIMEOUT_SECONDS)];
  // perl is on every macOS by default and most Linux distros — alarm() based fallback.
  if (hasOnPath('perl')) return ['perl', '-e', `alarm ${TEST_TIMEOUT_SECONDS}; exec @ARGV`];
  // Last resort: no timeout. The shim itself has internal timeouts per provider.
  return [];
}

/** Run the shim once with the chosen provider/model + a tiny canary input. */
export async function runShimTest(
  provider: DigestProvider,
  model: string,
  shimPath: string,
): Promise<TestResult> {
  const t0 = Date.now();
  const timeoutPrefix = pickTimeoutPrefix();
  // Source the shim and call its public function with the canary system prompt.
  // The bash script body is a fixed string; the prompt comes in via $1 from
  // the argv, so neither `provider`, `model`, nor the prompt text is ever
  // interpolated into a shell command line — they ride through env or argv.
  const bashScript = '. "$1"; digest_llm_invoke "$2"';
  const argv = [...timeoutPrefix, 'bash', '-c', bashScript, '_', shimPath, CANARY_PROMPT];
  const cmd = argv[0]!;
  const args = argv.slice(1);

  return new Promise<TestResult>((resolve) => {
    let child;
    try {
      child = spawn(cmd, args, {
        env: {
          ...process.env,
          IMMORTERM_DIGEST_PROVIDER: provider,
          IMMORTERM_DIGEST_MODEL: model,
        },
        stdio: ['pipe', 'pipe', 'pipe'],
      });
    } catch (e) {
      resolve({ ok: false, stdout: '', stderrTail: (e as Error).message ?? 'spawn failed', durationMs: Date.now() - t0 });
      return;
    }

    let stdout = '';
    let stderr = '';
    child.stdout?.on('data', (d) => { stdout += d.toString(); });
    child.stderr?.on('data', (d) => { stderr += d.toString(); });

    child.on('error', (err) => {
      resolve({ ok: false, stdout, stderrTail: tail(stderr || err.message, 200), durationMs: Date.now() - t0 });
    });

    child.on('close', (code) => {
      const durationMs = Date.now() - t0;
      const ok = code === 0 && stdout.trim().length > 0 && /\"result\"/.test(stdout);
      resolve({ ok, stdout, stderrTail: tail(stderr, 200), durationMs });
    });

    // Feed the canary prompt and close stdin.
    try {
      child.stdin?.write(CANARY_INPUT);
      child.stdin?.end();
    } catch { /* the close handler will report the error */ }
  });
}

function tail(s: string, n: number): string {
  if (!s) return '';
  return s.length <= n ? s : s.slice(s.length - n);
}

type FinalAction = 'save' | 'cancel';

async function showTestStep(
  provider: DigestProvider,
  model: string,
): Promise<{ action: FinalAction; validated: boolean }> {
  const shimPath = resolveShimPath();
  let validated = false;

  // Loop: user can Test repeatedly, then Save or Cancel.
  for (;;) {
    const choice = await vscode.window.showInformationMessage(
      `Digest LLM: ${model} via ${provider}`,
      { modal: true, detail: 'Test the connection (recommended), or Save now and verify later.' },
      'Test connection',
      'Save',
    );

    if (!choice) return { action: 'cancel', validated };
    if (choice === 'Save') return { action: 'save', validated };

    // choice === 'Test connection'
    const result = await vscode.window.withProgress(
      {
        location: vscode.ProgressLocation.Notification,
        title: `Testing ${provider} / ${model}…`,
        cancellable: false,
      },
      async () => runShimTest(provider, model, shimPath),
    );

    if (result.ok) {
      validated = true;
      const seconds = (result.durationMs / 1000).toFixed(1);
      const next = await vscode.window.showInformationMessage(
        `OK — got response in ${seconds}s — ${model} via ${provider}`,
        { modal: false },
        'Save',
        'Test again',
        'Cancel',
      );
      if (!next || next === 'Cancel') return { action: 'cancel', validated };
      if (next === 'Save') return { action: 'save', validated };
      // 'Test again' — fall through to the loop.
    } else {
      const next = await vscode.window.showErrorMessage(
        `Failed: ${result.stderrTail || 'no stderr captured'}`,
        { modal: true, detail: `Provider: ${provider}\nModel: ${model}` },
        'Retry',
        'Save anyway',
      );
      if (!next) return { action: 'cancel', validated };
      if (next === 'Save anyway') return { action: 'save', validated };
      // 'Retry' — fall through.
    }
  }
}

// ── Persistence ─────────────────────────────────────────────────

/** Write `services.digest.{provider, model}` into the per-project config. */
export function persistDigestChoice(workspacePath: string, choice: DigestLlmChoice): void {
  const existing = readProjectConfig(workspacePath);
  const cfg: ProjectConfig = existing ?? {
    version: 3,
    projectId: '',
    services: {
      memory: { enabled: false, graph: false },
      mcpGateway: { enabled: false },
      vendors: {
        claudeCode: { enabled: true },
        codex: { enabled: true },
        cursor: { enabled: true },
        windsurf: { enabled: true },
        cline: { enabled: true },
        opencode: { enabled: true },
        gemini: { enabled: true },
        aider: { enabled: true },
        copilot: { enabled: true },
      },
    },
  };
  cfg.services = {
    ...cfg.services,
    digest: { provider: choice.provider, model: choice.model },
  };
  writeProjectConfig(workspacePath, cfg);
}

// ── Public entry point ─────────────────────────────────────────

/**
 * Run the three-step picker. Returns the chosen provider/model
 * (with `validated: true` only when Test succeeded in this run),
 * or `undefined` if the user cancelled at any step.
 *
 * Side effect on success: writes `services.digest.{provider, model}`
 * to `<workspacePath>/.immorterm/config.json`.
 */
export async function pickDigestLlm(opts: PickDigestLlmOpts): Promise<DigestLlmChoice | undefined> {
  // Step 1
  const provider = await pickProvider(opts.initialProvider);
  if (!provider) return undefined;

  // Step 2
  const model = await pickModel(provider, opts.initialModel);
  if (!model) return undefined;

  // Step 3
  const { action, validated } = await showTestStep(provider, model);
  if (action !== 'save') return undefined;

  const choice: DigestLlmChoice = { provider, model, validated };
  persistDigestChoice(opts.workspacePath, choice);
  return choice;
}


// ── Test-only exports ──────────────────────────────────────────
// These are exported for unit tests; they are not part of the
// public picker contract.
export const __test__ = {
  PROVIDERS,
  modelsForProvider,
  pickProvider,
  pickModel,
  showTestStep,
};
