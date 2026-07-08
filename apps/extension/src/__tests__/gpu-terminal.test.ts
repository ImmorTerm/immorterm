import { describe, it, expect, vi, beforeEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';

// ── Mocks ────────────────────────────────────────────────────────

// Mock vscode (must be before any import that uses it)
const mockPostMessage = vi.fn();
const mockWebview = {
  postMessage: mockPostMessage,
  options: {},
  html: '',
  cspSource: 'https://mock.vscode-cdn.net',
  asWebviewUri: (uri: { fsPath: string }) => ({ toString: () => `https://mock.webview/${path.basename(uri.fsPath)}` }),
};
vi.mock('vscode', () => ({
  workspace: {
    getConfiguration: vi.fn().mockReturnValue({
      get: vi.fn((_key: string, def: unknown) => def),
      inspect: vi.fn(),
      update: vi.fn(),
    }),
    workspaceFolders: [{ uri: { fsPath: '/mock/project' } }],
    onDidChangeConfiguration: vi.fn(() => ({ dispose: vi.fn() })),
  },
  window: {
    showErrorMessage: vi.fn(),
    showInformationMessage: vi.fn(),
    createTerminal: vi.fn(),
  },
  env: { openExternal: vi.fn() },
  Uri: { file: vi.fn((p: string) => ({ fsPath: p, scheme: 'file' })) },
  commands: { executeCommand: vi.fn() },
  ConfigurationTarget: { Global: 1, Workspace: 2, WorkspaceFolder: 3 },
  ThemeIcon: class { constructor(public id: string, public color?: unknown) {} },
}));

// Mock logger
vi.mock('../utils/logger', () => ({
  logger: {
    info: vi.fn(),
    warn: vi.fn(),
    error: vi.fn(),
    debug: vi.fn(),
  },
}));

// Mock child_process (test-only mock, not production usage)
vi.mock('child_process', () => ({
  exec: vi.fn(),
  execFile: vi.fn(),
  spawn: vi.fn().mockReturnValue({ unref: vi.fn(), on: vi.fn(), stdout: null, stderr: null }),
}));

// Mock utils/process
vi.mock('../utils/process', () => ({
  generateWindowId: vi.fn().mockReturnValue('mock-window-id'),
}));

// Mock registry-client
vi.mock('../registry-client', () => ({
  updateRegistryNameAndCommand: vi.fn(),
  updateRegistryTitleLocked: vi.fn(),
  removeTerminalFromRegistry: vi.fn(),
}));

// Mock screen-commands
vi.mock('../utils/screen-commands', () => ({
  getDescendantPids: vi.fn().mockResolvedValue([]),
  killDescendants: vi.fn(),
  findClaudePidInTree: vi.fn().mockResolvedValue(null),
}));

// Mock immorterm-config — use hoisted mocks for getAppearance/updateAppearance
const { mockGetAppearance, mockUpdateAppearance } = vi.hoisted(() => ({
  mockGetAppearance: vi.fn().mockReturnValue({
    borderEnabled: true,
    borderOpacity: 1.0,
    statusBarEnabled: true,
    statusBarAnimations: true,
    statusBarMode: 'always',
  }),
  mockUpdateAppearance: vi.fn(),
}));

vi.mock('../utils/immorterm-config', () => ({
  getLogsDir: vi.fn().mockReturnValue('/mock/logs'),
  getRenamesDir: vi.fn().mockReturnValue('/mock/renames'),
  readGlobalConfig: vi.fn().mockReturnValue({}),
  writeGlobalConfig: vi.fn(),
  isServiceEnabled: vi.fn().mockReturnValue(false),
  setServiceEnabled: vi.fn(),
  getLicenseStatus: vi.fn().mockReturnValue({ tier: 'free' }),
  isProTier: vi.fn().mockReturnValue(false),
  getTheme: vi.fn().mockReturnValue(undefined),
  setTheme: vi.fn(),
  getAppearance: mockGetAppearance,
  updateAppearance: mockUpdateAppearance,
}));

// Mock mcp-gateway
vi.mock('../services/mcp-gateway', () => ({
  cleanupGatewaySessionByPid: vi.fn(),
  checkGatewayHealth: vi.fn().mockResolvedValue(false),
  getMCPGatewayState: vi.fn().mockReturnValue(null),
  startGateway: vi.fn(),
  stopGateway: vi.fn(),
}));

// Mock memory services
vi.mock('../services/memory', () => ({
  checkOpenMemoryHealth: vi.fn().mockResolvedValue(false),
  startOpenMemory: vi.fn(),
  stopOpenMemory: vi.fn(),
  getOpenMemoryState: vi.fn().mockReturnValue(null),
  isMemoryEnabled: vi.fn().mockReturnValue(false),
  isGraphEnabled: vi.fn().mockReturnValue(false),
  getStableProjectId: vi.fn().mockReturnValue('mock-project-id'),
}));

// Mock @immorterm/menu-data
vi.mock('@immorterm/menu-data', () => ({
  MENU_ITEMS: [],
  SERVICE_DEFS: [],
  LICENSE_ITEMS_PRO: [],
  LICENSE_ITEMS_FREE: [],
  THEME_DEFS: [],
  THEME_NAMES: [],
  FREE_THEME_NAMES: [],
}));

// Mock settings — gpu-terminal.ts no longer imports appearance functions from here
vi.mock('../utils/settings', () => ({
  getConfig: vi.fn((key: string) => {
    const defaults: Record<string, unknown> = {
      statusBarEnabled: true,
      terminalMode: 'regular',
    };
    return defaults[key];
  }),
  updateConfig: vi.fn().mockResolvedValue(undefined),
  SETTINGS: {
    STATUS_BAR_ENABLED: 'statusBarEnabled',
    TERMINAL_MODE: 'terminalMode',
    SCROLLBACK_BUFFER: 'scrollbackBuffer',
    HISTORY_ON_ATTACH: 'historyOnAttach',
    TERMINAL_RESTORE_DELAY: 'terminalRestoreDelay',
    RESTORE_ON_STARTUP: 'restoreOnStartup',
    CLOSE_EXISTING_ON_RESTORE: 'closeExistingOnRestore',
    MAX_LOG_SIZE_MB: 'maxLogSizeMb',
    LOG_RETAIN_LINES: 'logRetainLines',
    ENABLE_DEBUG_LOG: 'enableDebugLog',
    CLOSE_GRACE_PERIOD: 'closeGracePeriod',
    CLOSE_ACTION: 'closeAction',
    SHELVED_SESSION_TTL: 'shelvedSessionTtl',
    AUTO_CLEANUP_STALE: 'autoCleanupStale',
    NAMING_PATTERN: 'namingPattern',
    CLAUDE_AUTO_RESUME: 'claudeAutoResume',
    CLAUDE_SYNC_INTERVAL: 'claudeSyncInterval',
    CLAUDE_IDLE_TIMEOUT: 'claudeIdleTimeout',
  },
  isStatusBarEnabled: vi.fn().mockReturnValue(true),
}));

// ── Helpers ──────────────────────────────────────────────────────

const HTML_PATH = path.resolve(__dirname, '../../resources/gpu-terminal.html');
const MODALS_PATH = path.resolve(__dirname, '../../resources/gpu-terminal-modals.js');

function readHtml(): string {
  return fs.readFileSync(HTML_PATH, 'utf-8');
}

function readModals(): string {
  return fs.readFileSync(MODALS_PATH, 'utf-8');
}

/**
 * Extract the contents of the inline <script type="module"> block from the HTML.
 * Returns just the JS source (without script tags).
 */
function extractInlineScript(html: string): string {
  const match = html.match(/<script type="module">([\s\S]*?)<\/script>/);
  if (!match) throw new Error('No inline <script type="module"> found in gpu-terminal.html');
  return match[1];
}

/**
 * Extract all `terminal.methodName(` calls from the JS source.
 * Returns unique method names sorted alphabetically.
 */
function extractTerminalMethodCalls(js: string): string[] {
  const regex = /terminal\.(\w+)\s*\(/g;
  const methods = new Set<string>();
  let m;
  while ((m = regex.exec(js)) !== null) {
    methods.add(m[1]);
  }
  return [...methods].sort();
}

/**
 * Extract all pub fn names from the Rust WASM lib.rs that have #[wasm_bindgen].
 * Reads the actual Rust source file.
 */
function extractWasmExports(): string[] {
  const rustPath = path.resolve(__dirname, '../../../immorterm-ai/immorterm-wasm/src/lib.rs');
  const rust = fs.readFileSync(rustPath, 'utf-8');
  const lines = rust.split('\n');
  const exports = new Set<string>();

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!line.includes('#[wasm_bindgen]')) continue;

    // Case A: attr immediately before `impl Foo { ... }` — every pub fn
    // in the block is auto-exported. Scan until the matching closing }.
    // Brace-balance works because Rust impls nest but pub fn bodies
    // always open their own balanced braces — depth returns to 0 at the
    // impl's close.
    const implIdx = findNext(lines, i + 1, 3, /^\s*(pub\s+)?impl\b/);
    if (implIdx !== -1) {
      let depth = 0;
      let opened = false;
      for (let j = implIdx; j < lines.length; j++) {
        for (const ch of lines[j]) {
          if (ch === '{') { depth++; opened = true; }
          else if (ch === '}') depth--;
        }
        const fnMatch = lines[j].match(/pub\s+(?:async\s+)?fn\s+(\w+)/);
        if (fnMatch && opened && depth >= 1) exports.add(fnMatch[1]);
        if (opened && depth === 0) { i = j; break; }
      }
      continue;
    }

    // Case B: attr before a free pub fn (not inside an impl). Look at
    // the next few lines for `pub fn name(`.
    for (let j = i + 1; j < Math.min(i + 5, lines.length); j++) {
      const fnMatch = lines[j].match(/pub\s+(?:async\s+)?fn\s+(\w+)/);
      if (fnMatch) { exports.add(fnMatch[1]); break; }
    }
  }

  return [...exports].sort();
}

function findNext(lines: string[], start: number, window: number, re: RegExp): number {
  for (let k = start; k < Math.min(start + window, lines.length); k++) {
    if (re.test(lines[k])) return k;
  }
  return -1;
}

// ── Test Suites ──────────────────────────────────────────────────

describe('GPU Terminal — HTML Structure', () => {
  let html: string;

  beforeEach(() => {
    html = readHtml();
  });

  it('has a terminal canvas element', () => {
    expect(html).toContain('id="terminal-canvas"');
  });

  it('has a sidebar element', () => {
    expect(html).toContain('id="sidebar"');
  });

  it('has the container layout', () => {
    expect(html).toContain('id="container"');
    expect(html).toContain('id="terminal-area"');
  });

  it('has the empty state element', () => {
    expect(html).toContain('id="empty-state"');
  });

  it('has an inline module script', () => {
    expect(html).toMatch(/<script type="module">/);
  });

  it('has the error reporting function', () => {
    expect(html).toContain('reportError');
    // Error display uses toast pattern (not overlay) — created via createReportError factory
    expect(html).toContain('createReportError');
  });

  it('has window.onerror and unhandledrejection handlers', () => {
    expect(html).toContain('window.onerror');
    expect(html).toContain('window.onunhandledrejection');
  });

  it('has the renderLoop function with error handling', () => {
    expect(html).toContain('function renderLoop()');
    expect(html).toContain('renderErrorCount');
  });
});

describe('GPU Terminal — Inline JS Syntax', () => {
  it('extracts a non-empty script block', () => {
    const html = readHtml();
    const js = extractInlineScript(html);
    expect(js.length).toBeGreaterThan(1000);
  });

  // 20s: char-walks a ~1MB script block — CI runners exceed the default 5s
  it('has balanced braces in the script block', { timeout: 20_000 }, () => {
    const html = readHtml();
    const js = extractInlineScript(html);

    let depth = 0;
    for (const ch of js) {
      if (ch === '{') depth++;
      if (ch === '}') depth--;
      expect(depth).toBeGreaterThanOrEqual(0);
    }
    expect(depth).toBe(0);
  });

  it('passes V8 syntax check (no parse errors)', () => {
    const html = readHtml();
    const js = extractInlineScript(html);

    // Use node's vm module to syntax-check without executing.
    // This catches SyntaxErrors like unmatched parens/braces, unexpected tokens, etc.
    // Wrap in async function since the script uses top-level await.
    const { Script } = require('vm');
    const wrapped = `(async function() {\n${js.replace(/\bimport\s*\(/g, 'void(')}\n})`;
    try {
      new Script(wrapped, { filename: 'gpu-terminal-inline.js' });
    } catch (e: any) {
      throw new Error(`JS syntax error in gpu-terminal.html: ${e.message}`);
    }
  });
});

describe('GPU Terminal — WASM Binding Contract', () => {
  let jsMethods: string[];
  let wasmExports: string[];

  beforeEach(() => {
    const html = readHtml();
    const js = extractInlineScript(html);
    jsMethods = extractTerminalMethodCalls(js);
    wasmExports = extractWasmExports();
  });

  it('extracts JS method calls', () => {
    expect(jsMethods.length).toBeGreaterThan(10);
  });

  it('extracts WASM exports', () => {
    expect(wasmExports.length).toBeGreaterThan(10);
  });

  it('every JS terminal.method() call has a corresponding WASM export', () => {
    // `new` is the constructor — not a pub fn method
    const exceptions = new Set(['new']);

    const missing: string[] = [];
    for (const method of jsMethods) {
      if (exceptions.has(method)) continue;
      if (!wasmExports.includes(method)) {
        missing.push(method);
      }
    }

    if (missing.length > 0) {
      throw new Error(
        `JS calls terminal.${missing.join('(), terminal.')}() but these are NOT exported from WASM.\n` +
        `This will cause runtime errors (blank screen).\n` +
        `WASM exports: ${wasmExports.join(', ')}`
      );
    }
  });

  it('critical WASM methods are exported', () => {
    const critical = [
      'init_gpu',
      'render',
      'process',
      'resize',
      'dimensions',
      'handle_key',
      'set_theme',
      'set_status_bar_enabled',
      'set_border_enabled',
      'set_border_opacity',
      'set_animations_enabled',
      'visible_rows',
      'cell_size_device',
      'set_font_size',
      'set_line_height',
      'set_content_padding',
      'set_project_name',
      'set_session_title',
    ];

    const missing = critical.filter(m => !wasmExports.includes(m));
    expect(missing).toEqual([]);
  });
});

describe('GPU Terminal — Message Protocol', () => {
  const html = readHtml();
  const js = extractInlineScript(html);

  it('handles wasm-init message', () => {
    expect(js).toContain("case 'wasm-init'");
  });

  it('handles menu-data message', () => {
    expect(js).toContain("case 'menu-data'");
  });

  it('handles preferences message', () => {
    expect(js).toContain("case 'preferences'");
    expect(js).toContain('prefs.borderEnabled');
    expect(js).toContain('prefs.borderOpacity');
    expect(js).toContain('prefs.statusBar');
    expect(js).toContain('prefs.animations');
  });

  it('handles add-session message', () => {
    expect(js).toContain("case 'add-session'");
    expect(js).toContain('msg.sessionName');
    expect(js).toContain('msg.wsPort');
  });

  it('handles remove-session message', () => {
    expect(js).toContain("case 'remove-session'");
  });

  it('handles focus message', () => {
    expect(js).toContain("case 'focus'");
  });

  it('handles switch-session message', () => {
    expect(js).toContain("case 'switch-session'");
  });

  it('sends loaded message on page load', () => {
    expect(js).toContain("type: 'loaded'");
  });

  it('sends get-preferences message (via modals module)', () => {
    const modals = readModals();
    expect(modals).toContain("type: 'get-preferences'");
  });

  it('sends save-preference messages with correct keys (via modals module)', () => {
    const modals = readModals();
    expect(modals).toContain("type: 'save-preference'");
    expect(modals).toContain("key: 'borderEnabled'");
    expect(modals).toContain("key: 'borderOpacity'");
    expect(modals).toContain("key: 'statusBarMode'");
    expect(modals).toContain("key: 'statusBarAnimations'");
  });

  it('sends theme-changed message', () => {
    expect(js).toContain("type: 'theme-changed'");
  });

  it('sends error messages', () => {
    expect(js).toContain("type: 'error'");
  });
});

describe('GPU Terminal — Extension Message Handlers', () => {
  let provider: InstanceType<typeof import('../gpu-terminal').ImmorTermViewProvider>;
  let messageHandler: (msg: Record<string, unknown>) => void;

  beforeEach(async () => {
    vi.clearAllMocks();
    mockPostMessage.mockClear();

    const mockContext = {
      extensionPath: path.resolve(__dirname, '../..'),
      subscriptions: [],
      globalState: { get: vi.fn(), update: vi.fn() },
      workspaceState: { get: vi.fn(), update: vi.fn() },
    };

    const { ImmorTermViewProvider } = await import('../gpu-terminal');
    provider = new ImmorTermViewProvider(mockContext as any, 'test-project', '/mock/project');

    // Resolve the webview to capture the message handler
    const mockView = {
      webview: {
        ...mockWebview,
        postMessage: mockPostMessage,
        onDidReceiveMessage: (handler: Function) => {
          messageHandler = handler as (msg: Record<string, unknown>) => void;
          return { dispose: vi.fn() };
        },
      },
      onDidDispose: vi.fn(() => ({ dispose: vi.fn() })),
      onDidChangeVisibility: vi.fn(() => ({ dispose: vi.fn() })),
    };

    provider.resolveWebviewView(mockView as any, {} as any, {} as any);
  });

  it('responds to get-preferences with all 5 appearance settings', () => {
    mockGetAppearance.mockReturnValue({
      borderEnabled: true,
      borderOpacity: 0.8,
      statusBarEnabled: true,
      statusBarAnimations: false,
      statusBarMode: 'auto',
    });

    messageHandler({ type: 'get-preferences' });

    expect(mockPostMessage).toHaveBeenCalledWith({
      type: 'preferences',
      borderEnabled: true,
      borderOpacity: 0.8,
      statusBar: true,
      statusBarMode: 'auto',
      animations: false,
    });
  });

  it('persists save-preference for borderEnabled via updateAppearance', () => {
    messageHandler({ type: 'save-preference', key: 'borderEnabled', value: false });
    expect(mockUpdateAppearance).toHaveBeenCalledWith({ borderEnabled: false });
  });

  it('persists save-preference for borderOpacity via updateAppearance', () => {
    messageHandler({ type: 'save-preference', key: 'borderOpacity', value: 0.5 });
    expect(mockUpdateAppearance).toHaveBeenCalledWith({ borderOpacity: 0.5 });
  });

  it('persists save-preference for statusBarEnabled via updateAppearance', () => {
    messageHandler({ type: 'save-preference', key: 'statusBarEnabled', value: false });
    expect(mockUpdateAppearance).toHaveBeenCalledWith({ statusBarEnabled: false });
  });

  it('persists save-preference for statusBarAnimations via updateAppearance', () => {
    messageHandler({ type: 'save-preference', key: 'statusBarAnimations', value: false });
    expect(mockUpdateAppearance).toHaveBeenCalledWith({ statusBarAnimations: false });
  });

  it('ignores save-preference with unknown key', () => {
    messageHandler({ type: 'save-preference', key: 'unknownKey', value: true });
    expect(mockUpdateAppearance).not.toHaveBeenCalled();
  });

  it('handles error messages from webview', async () => {
    const vscode = await import('vscode');
    messageHandler({ type: 'error', message: 'Test error', stack: 'at test:1' });
    expect(vscode.window.showErrorMessage).toHaveBeenCalledWith('ImmorTerm AI Error: Test error');
  });
});

describe('GPU Terminal — CSP Injection', () => {
  it('raw HTML does not contain CSP meta tag (injected by extension)', () => {
    const html = readHtml();
    expect(html).not.toContain('Content-Security-Policy');
    expect(html).toContain('<script type="module">');
  });
});

describe('GPU Terminal — Preference Variables', () => {
  const html = readHtml();
  const js = extractInlineScript(html);

  it('declares prefs object with defaults', () => {
    expect(js).toMatch(/const\s+prefs\s*=\s*\{/);
    expect(js).toContain("borderEnabled: true");
    expect(js).toContain("borderOpacity: 1.0");
    expect(js).toContain("statusBarMode: 'always'");
    expect(js).toContain("animations: true");
  });

  it('has applyPrefs() helper called after init_gpu', () => {
    // applyPrefs is the single source of truth for applying prefs to WASM
    expect(js).toContain('function applyPrefs()');
    const initSection = js.slice(js.indexOf('init_gpu'), js.indexOf('init_gpu') + 500);
    expect(initSection).toContain('applyPrefs()');
  });

  it('applyPrefs sets all WASM methods', () => {
    const applyBody = js.slice(js.indexOf('function applyPrefs()'), js.indexOf('function applyPrefs()') + 800);
    expect(applyBody).toContain('set_border_enabled(prefs.borderEnabled)');
    expect(applyBody).toContain('set_border_opacity(prefs.borderOpacity)');
    expect(applyBody).toContain('set_status_bar_mode(prefs.statusBarMode)');
    expect(applyBody).toContain("'set_animations_enabled', prefs.animations");
    expect(applyBody).toContain("'set_expression_effects', prefs.expressionEffects");
    expect(applyBody).toContain("'set_celebrations_enabled', prefs.celebrations");
    expect(applyBody).toContain("'set_danger_effects', prefs.dangerEffects");
    expect(applyBody).toContain("'set_text_animations', prefs.textAnimations");
  });
});

describe('GPU Terminal — Appearance Config', () => {
  it('getAppearance returns all 5 appearance keys', async () => {
    const { getAppearance } = await import('../utils/immorterm-config');
    const appearance = getAppearance();
    expect(appearance).toHaveProperty('borderEnabled');
    expect(appearance).toHaveProperty('borderOpacity');
    expect(appearance).toHaveProperty('statusBarEnabled');
    expect(appearance).toHaveProperty('statusBarAnimations');
    expect(appearance).toHaveProperty('statusBarMode');
  });
});

describe('GPU Terminal — Render Loop Safety', () => {
  const html = readHtml();
  const js = extractInlineScript(html);

  it('has try-catch around terminal.render()', () => {
    const renderLoopMatch = js.match(/function renderLoop\(\)\s*\{([\s\S]*?)\n\s{4}\}/);
    expect(renderLoopMatch).not.toBeNull();

    const renderLoopBody = renderLoopMatch![1];
    expect(renderLoopBody).toContain('try');
    expect(renderLoopBody).toContain('catch');
    expect(renderLoopBody).toContain('terminal.render()');
    expect(renderLoopBody).toContain('reportError');
  });

  it('stops render loop after too many errors', () => {
    expect(js).toMatch(/renderErrorCount\s*>\s*10/);
    expect(js).toContain('return; // stop rAF chain');
  });

  it('resets error count on successful init', () => {
    expect(js).toContain('renderErrorCount = 0');
  });
});
