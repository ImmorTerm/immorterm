/**
 * ImmorTerm — WebviewView-based terminal using WASM + WebGPU.
 *
 * Uses WebviewView (not WebviewPanel) so the terminal appears in the
 * **bottom panel area** next to Terminal/Output/etc, not as an editor tab.
 *
 * Multi-session: the single webview manages multiple daemon sessions via
 * internal tab sidebar (like VS Code's built-in terminal panel).
 *
 * Architecture:
 *   Extension registers WebviewViewProvider → VS Code shows view in panel
 *   → command spawns daemon → reads .ws port → sends add-session to webview
 *   → webview creates WS connection → subscribe_raw → binary PTY frames
 *   → WASM Terminal processes bytes → GPU renders at 60fps
 */

import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import * as https from 'https';
import * as http from 'http';
import * as os from 'os';
import * as readline from 'readline';
import { execFile, spawn } from 'child_process';
import { logger } from './utils/logger';
import { generateWindowId } from './utils/process';
import { updateRegistryNameAndCommand, updateRegistryTitleLocked, updateRegistrySessionOrder, removeTerminalFromRegistry, removeSessionStatus, getRegistryTheme, updateRegistryTheme, updateSessionStatus, updateSessionSpeakMode, getSessionSpeakMode, markSpeakModeReset, getCurrentClaudeSessionId, getRegistryEntryByWindowId, getShelvedSessions, getClaudeResumeId, getClaudeExplicitlyExited, setActiveTerminal, getActiveTerminal, updateClaudeSessionId, removeClaudeSessionId, moveToShelvedRegistry, moveFromShelvedRegistry, removeShelvedRegistryEntry, readProjectId, resolveOwnerProjectFromPath } from './registry-client';
import { getDescendantPids, killDescendants, findClaudePidInTree } from './utils/screen-commands';
import { auditedKill } from './utils/kill-audit';
import {
  getLogsDir, getRenamesDir, readGlobalConfig, writeGlobalConfig,
  setServiceEnabled, getLicenseStatus, isProTier,
  getTheme, setTheme, getSpeakMode, setSpeakMode, getAppearance, getRawAppearance, updateAppearance,
  getProjectScreenrcPath,
} from './utils/immorterm-config';
import type { AppearanceConfig } from './utils/immorterm-config';
import {
  cleanupGatewaySessionByPid,
  checkGatewayHealth, getMCPGatewayState, startGateway, stopGateway,
} from './services/mcp-gateway';
import {
  checkOpenMemoryHealth, startOpenMemory, stopOpenMemory, getOpenMemoryState,
  isMemoryEnabled, isGraphEnabled,
  getStableProjectId,
} from './services/memory';
import {
  MENU_ITEMS, SERVICE_DEFS, LICENSE_ITEMS_PRO, LICENSE_ITEMS_FREE,
  THEME_DEFS, THEME_NAMES, FREE_THEME_NAMES,
  CHARACTER_DEFS, DEFAULT_CHARACTER_ID,
} from '@immorterm/menu-data';
import { HUB_PORT } from './hub-sidecar';
import { applyThemeToAllScreenSessions } from './terminal/restoration';
import { getTheme as getThemeObject, generateHardstatus } from './themes';
import { TaskStorage } from './tasks';
import type { Task, TaskContext } from './tasks/types';

const SOCKET_DIR = path.join(process.env.HOME || '~', '.immorterm', 'sockets');
const WASM_RESOURCE_DIR = 'resources/wasm';

export const VIEW_ID = 'immorterm.terminalView';

/**
 * Provides the ImmorTerm GPU terminal as a WebviewView in the bottom panel.
 *
 * Multi-session: each createSession() spawns a daemon and tells the webview
 * to add a new tab + WS connection. The webview manages switching internally.
 */
// ── Shiki syntax highlighting for link hover previews ──
const SHIKI_EXT_TO_LANG: Record<string, string> = {
  ts: 'typescript', tsx: 'tsx', js: 'javascript', jsx: 'jsx', mjs: 'javascript', cjs: 'javascript',
  rs: 'rust', py: 'python', go: 'go',
  json: 'json', jsonc: 'json',
  sh: 'bash', bash: 'bash', zsh: 'bash',
  md: 'markdown', markdown: 'markdown',
  html: 'html', htm: 'html',
  css: 'css', scss: 'scss',
  toml: 'toml', yml: 'yaml', yaml: 'yaml',
  c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', hpp: 'cpp',
  java: 'java', rb: 'ruby', php: 'php', sql: 'sql',
};
let shikiHighlighterPromise: Promise<any> | null = null;
const shikiLoadedLangs = new Set<string>();
async function getShikiHighlighter(): Promise<any> {
  if (!shikiHighlighterPromise) {
    shikiHighlighterPromise = (async () => {
      const shiki = await import('shiki');
      // Use pure-JS regex engine so the extension bundle has no WASM dependency.
      return shiki.createHighlighter({
        themes: ['github-dark'],
        langs: [],
        engine: shiki.createJavaScriptRegexEngine(),
      });
    })();
  }
  return shikiHighlighterPromise;
}
// Langs whose grammars carry state across lines (e.g. HTML switches into
// <script>/<style> sub-grammars). Shiki's `grammarContextCode` primes the
// tokenizer with a prefix so mid-file slices color correctly.
// https://shiki.style/guide/grammar-state
const STATEFUL_LANGS = new Set(['html', 'markdown', 'vue']);

async function tokenizeSnippet(
  code: string,
  ext: string | undefined,
  grammarContextCode?: string,
): Promise<Array<Array<{text: string; color?: string}>> | null> {
  if (!ext) return null;
  const lang = SHIKI_EXT_TO_LANG[ext];
  if (!lang) return null;
  try {
    const hl = await getShikiHighlighter();
    if (!shikiLoadedLangs.has(lang)) {
      await hl.loadLanguage(lang as any);
      shikiLoadedLangs.add(lang);
    }
    const opts: { lang: string; theme: string; grammarContextCode?: string } = { lang, theme: 'github-dark' };
    if (grammarContextCode) opts.grammarContextCode = grammarContextCode;
    const result = hl.codeToTokens(code, opts);
    return result.tokens.map((line: any[]) => line.map((t: any) => ({ text: t.content, color: t.color })));
  } catch (err) {
    logger.warn('[shiki] tokenize failed', String(err));
    return null;
  }
}

// Cache of full file text keyed by check-link-exists requestId, so lazy
// range-token requests from the webview can tokenize additional slices
// without re-reading the file. LRU-capped; dropped when tooltip dismisses
// implicitly via requestId turnover.
const previewTextCache = new Map<string, { text: string; ext: string | undefined }>();
const PREVIEW_CACHE_MAX = 12;
function rememberPreviewText(requestId: string, text: string, ext: string | undefined) {
  if (previewTextCache.has(requestId)) previewTextCache.delete(requestId);
  previewTextCache.set(requestId, { text, ext });
  while (previewTextCache.size > PREVIEW_CACHE_MAX) {
    const oldest = previewTextCache.keys().next().value;
    if (oldest === undefined) break;
    previewTextCache.delete(oldest);
  }
}

// Tokenize a 200-line window around targetLine. For stateful grammars (HTML
// embedding <script>/<style>, Markdown, Vue), we pass the file prefix as
// `grammarContextCode` so Shiki primes the tokenizer into the correct state
// before coloring the window. Lazy scrolling re-requests extra windows via
// `request-preview-range`.
const PREVIEW_WINDOW_LINES = 200;
async function tokenizeWindow(
  fullText: string,
  ext: string | undefined,
  targetLine: number | undefined,
): Promise<{ tokens: Array<Array<{text: string; color?: string}>>; startLine: number } | null> {
  if (!ext || !fullText) return null;
  const lang = SHIKI_EXT_TO_LANG[ext];
  if (!lang) return null;
  const lines = fullText.split('\n');
  const totalLines = lines.length;
  const target0 = targetLine && targetLine > 0 ? Math.min(totalLines - 1, targetLine - 1) : 0;
  const startIdx = Math.max(0, target0 - PREVIEW_WINDOW_LINES);
  const endIdx = Math.min(totalLines, target0 + PREVIEW_WINDOW_LINES);
  const chunk = lines.slice(startIdx, endIdx).join('\n');
  const prefix = STATEFUL_LANGS.has(lang) && startIdx > 0 ? lines.slice(0, startIdx).join('\n') : undefined;
  const tokens = await tokenizeSnippet(chunk, ext, prefix);
  if (!tokens) return null;
  return { tokens, startLine: startIdx + 1 };
}

export class ImmorTermViewProvider implements vscode.WebviewViewProvider {
  private view?: vscode.WebviewView;
  private context: vscode.ExtensionContext;
  private projectName: string;
  private projectPath: string;
  /** Owner project ID read from `<projectPath>/.immorterm/project-id` if
   *  present. Used as the primary key in the restore filter — survives
   *  project renames and machine moves. Lazily resolved at first restore. */
  private ownerProjectId: string | null = null;
  private sessions = new Map<string, { wsPort: number; displayName: string; windowId: string; titleLocked: boolean; needsAttention: boolean; daemonPid?: number; speakMode?: string; projectDir?: string; branch?: string }>();
  private wasmInitSent = false;
  private disposables: vscode.Disposable[] = [];
  /** Per-terminal share queue: one fs.watch per target windowId (deduped). */
  private shareWatchers = new Map<string, fs.FSWatcher>();
  /** Queued task items awaiting consume, so we link the task only when the
   *  hook actually consumes it (not on cancel). Keyed by itemId. */
  private pendingTaskItems = new Map<string, { taskId: string; targetWindowId: string; displayName: string }>();
  /** Per-project-dir git watchers. Each session has its OWN branch derived
   *  from its OWN project_dir's .git/HEAD — different sessions in the same
   *  VS Code window can be on different branches (trunk + worktrees).
   *  Watchers are keyed by project_dir so we don't double-watch the same
   *  .git for N sessions in the same project. */
  private gitWatchers = new Map<string, fs.FSWatcher>();
  /** windowId of the currently-active sidebar tab, tracked from the
   *  webview's `session-switched` message. Used to know whose branch to
   *  push to the WASM renderer (the status-bar label is global to the
   *  webview, so only the active session's branch is shown at any time). */
  private activeWindowId: string | null = null;
  /** Last branch label we told the webview about — dedupe + no-op guard. */
  private lastSentBranch: string | undefined = undefined;

  /** Soft-shelve registry: window_id → preserved session metadata + daemon
   * info. The daemon is STILL ALIVE; we just removed it from the active
   * `sessions` map and from the webview tab strip. Reattach within the TTL
   * is an instant WS reconnect — no SIGTERM, no archive, no claude --resume.
   * After the TTL expires, the per-session timer fires hard shelve. Phase-1
   * MVP: this state is in-memory only, lost on extension restart (the daemon
   * survives but gets re-discovered as orphan and reaped). Phase 2 will
   * persist via session-status.json so VS Code restart preserves softs. */
  private softShelved = new Map<string, {
    sessionName: string;
    wsPort: number;
    displayName: string;
    titleLocked: boolean;
    daemonPid?: number;
    speakMode?: string;
    softShelvedAt: number;
    timer: NodeJS.Timeout;
  }>();

  // Queue session creation if command fires before view resolves
  private pendingSessionRequests: Array<{ name?: string; resolve: (ok: boolean) => void }> = [];

  // Set when restoreSessions() is in progress. The 'loaded' handler awaits
  // this to avoid snapshotting an empty/partial sessions Map.
  private restorePromise: Promise<void> | null = null;

  // Persisted active AI windowId — captured once from session-status.json before
  // any session-switched callbacks can overwrite it. Set by the first code path
  // that sends sessions to the webview (loaded handler or ensureSessionsSent).
  private persistedActiveAiWindowId: string | undefined;
  private persistedActiveAiCaptured = false;
  private taskStorage: TaskStorage | null = null;
  private knownTaskIds = new Set<string>();
  private initialFocusDone = false;

  // Paint-canary: tracks webview health for autonomous testing and auto-recovery.
  // Health file written to ~/.immorterm/ai-health.json for external tools to read.
  private lastCanary: Record<string, unknown> | null = null;
  private lastTitleBySession = new Map<string, string>(); // dedup title IPC
  private lastHealthWrite = 0; // throttle ai-health.json writes
  private canaryRecoveryCount = 0;
  private canaryWatchdog: ReturnType<typeof setTimeout> | undefined;
  private ensureSessionsTimer: ReturnType<typeof setTimeout> | undefined;
  private ensureSessionsInFlight = false;



  constructor(context: vscode.ExtensionContext, projectName: string, projectPath?: string) {
    this.context = context;
    this.projectName = projectName;
    this.projectPath = projectPath || '';
  }

  /** Called by VS Code when the view becomes visible for the first time. */
  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken,
  ): void {
    this.view = webviewView;
    logger.info('ImmorTerm AI: WebviewView resolved');


    // Initialize task storage (project-scoped)
    if (this.projectPath) {
      try {
        const projectId = getStableProjectId(this.projectPath);
        this.taskStorage = new TaskStorage(projectId);
        for (const t of this.taskStorage.list()) this.knownTaskIds.add(t.id);
        this.taskStorage.on('change', () => this.sendTasksToWebview());
        this.taskStorage.on('external-change', () => this.onExternalTaskChange());
      } catch (err) {
        logger.warn('ImmorTerm AI: failed to init task storage:', err);
      }
    }

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources')),
        vscode.Uri.file('/'),
        ...(vscode.workspace.workspaceFolders?.map((f) => f.uri) ?? []),
      ],
    };

    // Handle messages from the webview
    webviewView.webview.onDidReceiveMessage(
      (msg) => this.handleWebviewMessage(msg),
      null,
      this.disposables,
    );

    // Force re-render when the panel becomes visible again (e.g., tab switch).
    // Without this, the WebGPU surface is stale and the terminal appears distorted.
    webviewView.onDidChangeVisibility(() => {
      if (webviewView.visible) {
        webviewView.webview.postMessage({ type: 'resize' });
        webviewView.webview.postMessage({ type: 'visibility', visible: true });
        // Safety net: resend sessions when panel becomes visible.
        // Debounced 500ms to coalesce rapid visibility toggles into one burst.
        // The webview's addSession() skips duplicates, so this is harmless.
        if (this.ensureSessionsTimer) clearTimeout(this.ensureSessionsTimer);
        this.ensureSessionsTimer = setTimeout(() => {
          this.ensureSessionsTimer = undefined;
          if (!this.ensureSessionsInFlight) {
            this.ensureSessionsInFlight = true;
            this.ensureSessionsSent().finally(() => { this.ensureSessionsInFlight = false; });
          }
        }, 500);
      } else {
        webviewView.webview.postMessage({ type: 'visibility', visible: false });
      }
    }, null, this.disposables);

    // Handle view disposal
    webviewView.onDidDispose(() => {
      this.view = undefined;
      this.wasmInitSent = false;
      logger.info('ImmorTerm AI: WebviewView disposed');
    });

    // Set the HTML
    this.setWebviewHtml(webviewView.webview);
  }

  /** Send a message to the webview (e.g. toggle-pomodoro from keybinding). */
  postMessageToWebview(msg: { type: string; [key: string]: unknown }) {
    this.view?.webview.postMessage(msg);
  }

  /** Create a new ImmorTerm session. Reveals the view if hidden. */
  async createSession(name?: string): Promise<boolean> {
    // If view isn't resolved yet, focus it and queue the request
    if (!this.view) {
      logger.info('ImmorTerm AI: view not resolved, focusing and queuing session');
      await vscode.commands.executeCommand(`${VIEW_ID}.focus`);

      if (!this.view) {
        return new Promise<boolean>((resolve) => {
          this.pendingSessionRequests.push({ name, resolve });
          setTimeout(() => {
            const idx = this.pendingSessionRequests.findIndex(r => r.resolve === resolve);
            if (idx >= 0) {
              this.pendingSessionRequests.splice(idx, 1);
              resolve(false);
            }
          }, 5000);
        });
      }
    }

    this.view.show?.(true);
    return this.startSession(name);
  }

  /** Generate the next sequential display name (immorterm-1, immorterm-2, ...).
   *  This is the friendly name shown in the sidebar — not used for daemon identity.
   */
  private nextDisplayName(): string {
    let maxN = 0;
    for (const session of this.sessions.values()) {
      const match = session.displayName.match(/^immorterm-(\d+)$/i);
      if (match) {
        maxN = Math.max(maxN, parseInt(match[1], 10));
      }
    }
    return `immorterm-${maxN + 1}`;
  }

  /** Build the daemon session name from a windowId.
   *  Format: {project}-ai-{windowId} — unique and stable for registry lookup.
   */
  private buildAiSessionName(windowId: string): string {
    return `${this.projectName}-ai-${windowId}`;
  }

  /** Internal: spawn daemon and send add-session to webview.
   *  @param displayNameOverride Optional friendly name for the sidebar tab.
   */
  private async startSession(displayNameOverride?: string): Promise<boolean> {
    const windowId = generateWindowId();
    const sessionName = this.buildAiSessionName(windowId);
    const displayName = displayNameOverride || this.nextDisplayName();

    // Reserve the name immediately so concurrent startSession() calls
    // see it in this.sessions and generate the next sequential number.
    const newBranch = detectGitBranch(this.projectPath);
    this.sessions.set(sessionName, { wsPort: 0, displayName, windowId, titleLocked: false, needsAttention: false, daemonPid: undefined, projectDir: this.projectPath, branch: newBranch });
    this.armSessionBranchWatcher(this.projectPath);

    try {
      logger.info(`ImmorTerm AI: creating session '${sessionName}' (display: '${displayName}', windowId: '${windowId}')`);
      const binary = await findDaemonBinary();
      if (!binary) {
        vscode.window.showErrorMessage(
          'ImmorTerm AI: daemon binary not available. Ensure `gh` CLI is installed, or run: npx immorterm'
        );
        this.sessions.delete(sessionName);
        return false;
      }

      const spawnedPid = await spawnDaemon(binary, sessionName, this.projectPath, windowId, displayName);
      logger.info(`ImmorTerm AI: daemon spawning for '${sessionName}' (spawn pid: ${spawnedPid})`);

      // 8s timeout — double-forked daemon may take time under load (VS Code
      // restart, concurrent spawns). spawnedPid is passed for stale-file
      // guard but the exact match always fails (double-fork PID ≠ daemon PID);
      // the live-PID fallback in findWsPort handles discovery.
      const wsPort = await waitForWsPort(sessionName, 8000, spawnedPid);
      if (!wsPort) {
        logger.error(`ImmorTerm AI: daemon for '${sessionName}' did not start (no .ws port file after 8s)`);
        this.sessions.delete(sessionName);
        return false;
      }

      // The real daemon PID differs from spawnedPid due to double-fork.
      // Extract it from the .ws filename for correct kill-session behavior.
      const daemonPid = findDaemonPidFromWsFile(sessionName);

      logger.info(`ImmorTerm AI: daemon ready at ws://127.0.0.1:${wsPort} (daemon pid: ${daemonPid})`);
      this.sessions.set(sessionName, { wsPort, displayName, windowId, titleLocked: false, needsAttention: false, daemonPid, projectDir: this.projectPath, branch: newBranch });
      this.sendSessionToWebview(sessionName, wsPort, displayName);

      logger.info(`ImmorTerm AI: session '${sessionName}' added (display: '${displayName}')`);
      return true;
    } catch (err) {
      logger.error(`ImmorTerm AI: failed to create session: ${err}`);
      vscode.window.showErrorMessage(`ImmorTerm AI Error: ${err}`);
      this.sessions.delete(sessionName);
      return false;
    }
  }

  /**
   * Safety net: resend all sessions to the webview.
   * Called on visibility change to cover cases where the 'loaded' handler
   * failed silently. The webview's addSession() skips duplicates, so this
   * is harmless when sessions were already delivered.
   */
  private async ensureSessionsSent() {
    if (!this.view || this.sessions.size === 0) return;
    // Wait for restore if it's still in progress
    if (this.restorePromise) {
      try { await this.restorePromise; } catch { /* logged elsewhere */ }
    }

    logger.info(`ImmorTerm AI: ensureSessionsSent — resending ${this.sessions.size} sessions`);
    for (const [sessionName, session] of this.sessions) {
      if (!session.wsPort) continue;
      try {
        await this.sendSessionToWebview(sessionName, session.wsPort);
      } catch (err) {
        logger.error(`ImmorTerm AI: ensureSessionsSent failed for '${sessionName}': ${err}`);
      }
    }

    // Focus the persisted active AI session (uses cached capture, immune to overwrites)
    this.focusActiveAiSession(this.capturePersistedActiveAi());
  }

  /** Capture persisted active AI windowId once, before session-switched callbacks can overwrite it. */
  private capturePersistedActiveAi(): string | undefined {
    if (!this.persistedActiveAiCaptured) {
      this.persistedActiveAiWindowId = getActiveTerminal('ai');
      this.persistedActiveAiCaptured = true;
      logger.info(`ImmorTerm AI: captured persisted active AI windowId: ${this.persistedActiveAiWindowId || 'none'}`);
    }
    return this.persistedActiveAiWindowId;
  }

  /** Tell webview to switch to the last-active AI session (persisted across restarts). One-shot: only fires on initial restore. */
  private focusActiveAiSession(activeWindowId?: string) {
    if (this.initialFocusDone || !activeWindowId || !this.view) return;

    // Find the session name for this windowId
    for (const [sessionName, session] of this.sessions) {
      if (session.windowId === activeWindowId) {
        this.view.webview.postMessage({ type: 'focus-session', sessionName });
        this.initialFocusDone = true;
        logger.info(`ImmorTerm AI: focusing last-active session '${session.displayName}' (${activeWindowId})`);
        return;
      }
    }
  }

  /**
   * Send the wasm-init + menu-data + preferences trio to the webview.
   * Idempotent — guarded by `wasmInitSent`, so multiple callers won't
   * double-emit.
   *
   * Separated from `sendSessionToWebview` because a project with ZERO
   * sessions (e.g. lonormaly after `restoreSessions=disabled`) never
   * iterates the session loop and so never got wasm-init under the
   * old single-method design. The webview hung at boot-shield until
   * the 10s timeout. This method is now called UNCONDITIONALLY from
   * the 'loaded' handler so WASM always boots, even for empty
   * projects where the user can then create a new session.
   */
  private async sendWasmInitToWebview(): Promise<void> {
    if (!this.view) return;
    if (this.wasmInitSent) return;
    const webview = this.view.webview;
    {
      const wasmJsUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, WASM_RESOURCE_DIR, 'immorterm_wasm.js')),
      );
      const wasmBgUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, WASM_RESOURCE_DIR, 'immorterm_wasm_bg.wasm')),
      );
      const utilsUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal-utils.js')),
      );
      const modalsUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal-modals.js')),
      );
      const pomodoroUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal-pomodoro.js')),
      );
      const tasksUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal-tasks.js')),
      );
      const filesUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal-files.js')),
      );
      const browserUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal-browser.js')),
      );
      const markedUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'vendor', 'marked.umd.js')),
      );
      const iroUri = webview.asWebviewUri(
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'vendor', 'iro.min.js')),
      );
      // Read VS Code terminal settings
      const termConfig = vscode.workspace.getConfiguration('terminal.integrated');
      const editorConfig = vscode.workspace.getConfiguration('editor');
      const termFontSize = termConfig.get<number>('fontSize') || editorConfig.get<number>('fontSize') || 14;
      const termLineHeight = termConfig.get<number>('lineHeight') || 1; // VS Code default: 1 (means 1× font size)
      const termFontWeight = parseFontWeight(termConfig.get<string>('fontWeight') || 'normal');

      // Try to find the user's terminal font file for GPU rendering
      const fontResult = await findTerminalFontData();

      // Initial label: workspace base only \u2014 no branch suffix. The daemon
      // will push the active session's branch via control events as soon as
      // sessions connect, and the webview composes "<base> \u2387 <branch>" per
      // active tab. Pre-AI-session period (or never-have-AI-session) shows
      // the bare base label.
      const displayProjectName = this.projectName;

      webview.postMessage({
        type: 'wasm-init',
        wasmJsUri: wasmJsUri.toString(),
        wasmBgUri: wasmBgUri.toString(),
        utilsUri: utilsUri.toString(),
        modalsUri: modalsUri.toString(),
        pomodoroUri: pomodoroUri.toString(),
        tasksUri: tasksUri.toString(),
        filesUri: filesUri.toString(),
        browserUri: browserUri.toString(),
        markedUri: markedUri.toString(),
        iroUri: iroUri.toString(),
        // Hub URL for webview-side fetches/sockets. The VS Code webview
        // origin is the cdn (not the hub), so the HTML can't derive it from
        // location; we report it from the single TS-side source (HUB_PORT in
        // hub-sidecar). Standalone/Tauri ignore this and use location.origin.
        hubBaseUrl: `http://127.0.0.1:${HUB_PORT}`,
        projectName: displayProjectName,
        // Project root for the file browser — static across session switches.
        projectDir: this.projectPath,
        // Base form (no branch suffix). Webview keeps this and recomposes
        // the rendered label per active session's branch.
        projectNameBase: this.projectName,
        fontSize: termFontSize,
        lineHeight: termLineHeight,
        fontWeight: termFontWeight,
        fontData: fontResult?.data ?? null,
        fontName: fontResult?.name ?? null,
      });
      this.wasmInitSent = true;

      // Send shared menu/theme data so the webview doesn't hardcode it
      webview.postMessage({
        type: 'menu-data',
        menuItems: MENU_ITEMS,
        serviceDefs: SERVICE_DEFS,
        licenseItemsPro: LICENSE_ITEMS_PRO,
        licenseItemsFree: LICENSE_ITEMS_FREE,
        themes: THEME_NAMES.map(name => {
          const def = THEME_DEFS[name]!;
          return {
            name,
            label: def.label,
            desc: def.description,
            bg: def.statusBarStops[0],
            accent: def.fgAccent,
            fg: def.fg,
            stops: def.statusBarStops,
          };
        }),
        freeThemes: [...FREE_THEME_NAMES],
        projectTheme: getTheme(this.projectPath),
        characterDefs: CHARACTER_DEFS,
        projectSpeakMode: getSpeakMode(this.projectPath) ?? DEFAULT_CHARACTER_ID,
      });

      // Send visual preferences so the webview can apply them before first render
      const appearance = getAppearance();
      // View modes + rail layout go RAW (no default merge): undefined means
      // "no stored choice — webview state wins". A merged default would echo
      // 'show' and clobber a pre-upgrade collapse that lived only in webview
      // state. railsEnabled stays merged — its false default is the real
      // pre-flip default, not a clobber.
      const rawAppearance = getRawAppearance();
      webview.postMessage({
        type: 'preferences',
        borderEnabled: appearance.borderEnabled,
        borderOpacity: appearance.borderOpacity,
        statusBar: appearance.statusBarEnabled,
        statusBarMode: appearance.statusBarMode,
        animations: appearance.statusBarAnimations,
        expressionEffects: appearance.expressionEffects,
        celebrations: appearance.celebrations,
        dangerEffects: appearance.dangerEffects,
        textAnimations: appearance.textAnimations,
        sidebarMode: rawAppearance.sidebarMode,
        tasksMode: rawAppearance.tasksMode,
        workshopsMode: rawAppearance.workshopsMode,
        railsEnabled: appearance.railsEnabled,
        railLayout: rawAppearance.railLayout,
        viewModes: rawAppearance.viewModes,
        backgroundControlMode: appearance.backgroundControlMode,
      });

    }
  }

  /** Send WASM init (idempotent) + add-session message to the webview. */
  private async sendSessionToWebview(sessionName: string, wsPort: number, displayName?: string) {
    if (!this.view) {
      logger.warn(`ImmorTerm AI: sendSessionToWebview('${sessionName}') skipped — view not resolved yet`);
      return;
    }
    await this.sendWasmInitToWebview();
    const webview = this.view.webview;

    // Tell webview to add this session
    const resolvedDisplayName = displayName || this.sessions.get(sessionName)?.displayName || sessionName;

    // Theme cascade: per-terminal (registry) > per-project (config.json) > default
    const session = this.sessions.get(sessionName);
    const theme = (session?.windowId && getRegistryTheme(session.windowId))
               || getTheme(this.projectPath)
               || undefined;
    // Speak Mode per-session override persists in session-status.json.
    // Rehydrate into the in-memory session so the sidebar badge survives
    // VS Code reload (sessions Map gets rebuilt from scratch on activation).
    const speakMode = session?.windowId ? getSessionSpeakMode(session.windowId) : undefined;
    if (session && speakMode && !session.speakMode) {
      session.speakMode = speakMode;
    }

    webview.postMessage({
      type: 'add-session',
      sessionName,
      wsPort,
      displayName: resolvedDisplayName,
      theme,
      titleLocked: this.sessions.get(sessionName)?.titleLocked ?? false,
      needsAttention: this.sessions.get(sessionName)?.needsAttention ?? false,
      windowId: session?.windowId,
      speakMode,
    });
  }

  /** Set the webview HTML content with CSP. */
  private setWebviewHtml(webview: vscode.Webview) {
    const htmlPath = path.join(this.context.extensionPath, 'resources', 'gpu-terminal.html');
    let html = fs.readFileSync(htmlPath, 'utf-8');

    const nonce = getNonce();
    const cssUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'gpu-terminal.css')),
    );
    const codiconCssUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.context.extensionPath, 'resources', 'vendor', 'codicons', 'codicon.css')),
    );

    const csp = [
      `default-src 'none'`,
      `script-src 'nonce-${nonce}' 'unsafe-eval' 'wasm-unsafe-eval' ${webview.cspSource}`,
      `style-src 'unsafe-inline' ${webview.cspSource}`,
      // codicon.ttf is fetched by codicon.css via a relative url() —
      // same cspSource origin as the stylesheet itself.
      `font-src ${webview.cspSource}`,
      // connect-src needs to include the hub on 127.0.0.1:1440 so the
      // host-agnostic modals (digest LLM, etc.) can talk to it via
      // fetch(). Without HTTP allowed here, every webview HTTP call
      // is silently blocked and falls back to the webview\u2019s own
      // static-resource server, which returns 403 for non-GETs.
      `connect-src ws://127.0.0.1:* http://127.0.0.1:* http://localhost:*`,
      `img-src ${webview.cspSource} data: https:`,
      `media-src ${webview.cspSource} blob:`,
      // Link-preview HTML iframe loads from asWebviewUri (cspSource origin).
      // Without this, default-src='none' blocks the iframe entirely.
      // Google entries enable the explore-popup's embedded search via igu=1
      // (the unblocked-frame variant that doesn't set X-Frame-Options:DENY).
      `frame-src ${webview.cspSource} https://www.google.com https://*.google.com`,
    ].join('; ');

    html = html.replace('__CSS_URI__', cssUri.toString());
    html = html.replace('__CODICON_CSS_URI__', codiconCssUri.toString());
    html = html.replace(
      '<script type="module">',
      `<meta http-equiv="Content-Security-Policy" content="${csp}">\n  <script type="module" nonce="${nonce}">`,
    );

    webview.html = html;
  }

  /**
   * Paint-canary handler: receives structured health telemetry from the webview.
   * Writes health status to ~/.immorterm/ai-health.json so external tools
   * (Claude Code, CI scripts) can verify the webview is healthy after deploy.
   */
  private handlePaintCanary(msg: Record<string, unknown>) {
    this.lastCanary = msg;

    // Write health file for external verification (non-blocking)
    const healthPath = path.join(os.homedir(), '.immorterm', 'ai-health.json');
    const health = {
      timestamp: new Date().toISOString(),
      healthy: !!(msg.wasmReady && msg.renderLoopRunning && (msg.frameCount as number) > 0),
      canaryCount: msg.count,
      frameCount: msg.frameCount,
      errorCount: msg.errorCount,
      renderLoopRunning: msg.renderLoopRunning,
      wasmReady: msg.wasmReady,
      sessionCount: msg.sessionCount,
      wsConnected: msg.wsConnected,
      canvasPixels: msg.canvasPixels,
      activeSession: msg.activeSession,
      recoveryCount: this.canaryRecoveryCount,
    };
    // Throttle writes: every canary during startup (count <= 6), then every 30s
    const now = Date.now();
    const isStartup = (msg.count as number) <= 6;
    if (isStartup || now - this.lastHealthWrite >= 30000) {
      this.lastHealthWrite = now;
      fs.writeFile(healthPath, JSON.stringify(health, null, 2), () => {});
    }

    // Auto-recovery watchdog: if frames stay at 0 for >15s after init,
    // something may be wrong. Re-render the webview as a safety net.
    if (msg.wasmReady && (msg.frameCount as number) === 0 && (msg.count as number) <= 3) {
      if (!this.canaryWatchdog && this.canaryRecoveryCount < 2) {
        this.canaryWatchdog = setTimeout(() => {
          // Check if still no frames
          if (this.lastCanary && (this.lastCanary.frameCount as number) === 0) {
            logger.warn('ImmorTerm AI: paint-canary: 0 frames after 15s — triggering recovery');
            this.canaryRecoveryCount++;
            if (this.view) {
              this.setWebviewHtml(this.view.webview);
            }
          }
          this.canaryWatchdog = undefined;
        }, 15000);
      }
    } else if ((msg.frameCount as number) > 0 && this.canaryWatchdog) {
      clearTimeout(this.canaryWatchdog);
      this.canaryWatchdog = undefined;
    }
  }

  /** Per-terminal share queue directory — keyed by the CONSUMING terminal's
   *  stable windowId (== its IMMORTERM_ID). The hook only ever reads its own
   *  directory, so no other terminal can consume these items. */
  private shareQueueDir(targetWindowId: string): string {
    return path.join(os.homedir(), '.immorterm', 'pending-share', targetWindowId);
  }

  /** Write one share item into the target terminal's queue. Returns the
   *  generated itemId, or null when the target has no stable id (we refuse to
   *  write rather than risk a mis-scoped/colliding signal). */
  private writeShareItem(targetName: string, item: Record<string, unknown>): string | null {
    const target = this.sessions.get(targetName);
    if (!target || !target.windowId) return null;
    const dir = this.shareQueueDir(target.windowId);
    try { fs.mkdirSync(dir, { recursive: true }); } catch { /* exists */ }
    const itemId = `${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
    const payload = { id: itemId, timestamp: Date.now(), ...item };
    try {
      fs.writeFileSync(path.join(dir, `${itemId}.json`), JSON.stringify(payload));
    } catch (e) {
      logger.warn(`ImmorTerm AI: writeShareItem failed: ${e}`);
      return null;
    }
    this.ensureShareWatcher(target.windowId, targetName);
    return itemId;
  }

  /** One reconcile watcher per target queue dir. On any change it reports the
   *  remaining itemIds to the webview (which consume-animates the missing
   *  badges) and links any task whose item was CONSUMED (gone but not
   *  cancelled — cancel removes it from pendingTaskItems first). */
  private ensureShareWatcher(targetWindowId: string, targetName: string): void {
    if (this.shareWatchers.has(targetWindowId)) return;
    const dir = this.shareQueueDir(targetWindowId);
    let watcher: fs.FSWatcher;
    try {
      watcher = fs.watch(dir, () => {
        let ids: string[] = [];
        try {
          ids = fs.readdirSync(dir).filter(f => f.endsWith('.json')).map(f => f.slice(0, -5));
        } catch { ids = []; }
        this.view?.webview.postMessage({ type: 'share-queue-state', targetSession: targetName, itemIds: ids });
        // Link tasks that were consumed (no longer queued, still tracked).
        for (const [itemId, info] of [...this.pendingTaskItems]) {
          if (info.targetWindowId === targetWindowId && !ids.includes(itemId)) {
            this.taskStorage?.linkSession(info.taskId, targetWindowId, info.displayName);
            this.sendTasksToWebview();
            this.pendingTaskItems.delete(itemId);
          }
        }
        if (ids.length === 0) {
          watcher.close();
          this.shareWatchers.delete(targetWindowId);
        }
      });
    } catch (e) {
      logger.warn(`ImmorTerm AI: share watcher failed for ${targetWindowId}: ${e}`);
      return;
    }
    this.shareWatchers.set(targetWindowId, watcher);
    setTimeout(() => { watcher.close(); this.shareWatchers.delete(targetWindowId); }, 3600_000);
  }

  private handleWebviewMessage(msg: { type: string; [key: string]: unknown }) {
    switch (msg.type) {
      case 'loaded': {
        logger.info('ImmorTerm AI: webview loaded');
        // The webview HTML was (re-)rendered — WASM must be re-initialized.
        // This happens on first show, tab switch, or VS Code re-rendering the panel.
        this.wasmInitSent = false;
        // Send initial visibility state. The webview defaults webviewVisible=true,
        // but the panel may start hidden (e.g., TERMINAL tab is active). Without
        // this, rAF callbacks die silently while renderLoopRunning stays "true".
        if (this.view) {
          this.view.webview.postMessage({ type: 'visibility', visible: this.view.visible });
        }
        // Re-send all sessions sequentially. Must await restorePromise first —
        // restoreSessions() runs concurrently with view resolution, so 'loaded'
        // can fire before restore completes. Without this, we snapshot an
        // empty/partial sessions Map and the user sees no terminals.
        (async () => {
          if (this.restorePromise) {
            logger.info('ImmorTerm AI: loaded handler waiting for restore to complete...');
            await this.restorePromise;
            logger.info(`ImmorTerm AI: restore complete, sending ${this.sessions.size} sessions to webview`);
          }
          // Send wasm-init UNCONDITIONALLY, even when the project has zero
          // sessions to restore. Without this, projects opened for the
          // first time (or with terminal restoration disabled) saw the
          // webview hang at the boot-shield for 10s and time out — the
          // WASM module was never loaded because the only caller of
          // wasm-init was the session loop below, which is empty for
          // zero-session projects.
          await this.sendWasmInitToWebview();
          // Capture persisted active BEFORE sending sessions — session-switched
          // callbacks from the webview will overwrite getActiveTerminal('ai')
          const activeAiWindowId = this.capturePersistedActiveAi();
          const sessions = [...this.sessions.entries()];
          for (const [sessionName, session] of sessions) {
            // Skip sessions still waiting for daemon startup (wsPort: 0).
            // startSession() will send them once the real port is known.
            if (!session.wsPort) continue;
            await this.sendSessionToWebview(sessionName, session.wsPort);
          }
          logger.info(`ImmorTerm AI: loaded handler sent ${sessions.length} sessions to webview`);
          // Focus the persisted active AI session (captured before sending, immune to overwrites)
          this.focusActiveAiSession(activeAiWindowId);
          // Process any queued session requests
          if (this.pendingSessionRequests.length > 0) {
            const pending = [...this.pendingSessionRequests];
            this.pendingSessionRequests = [];
            for (const req of pending) {
              this.startSession(req.name).then(req.resolve);
            }
          }
        })().catch(err => {
          logger.error(`ImmorTerm AI: loaded handler failed: ${err}`);
        });
        break;
      }
      case 'ready':
        logger.info(`ImmorTerm AI: GPU terminal ready for ${msg.sessionName}`);
        // Re-send task counts now that this session has its windowId in the webview
        this.sendTasksToWebview();
        break;
      case 'error':
        logger.error(`ImmorTerm AI: GPU terminal error: ${msg.message}`);
        if (msg.stack) logger.error(`ImmorTerm AI: stack: ${msg.stack}`);
        vscode.window.showErrorMessage(`ImmorTerm AI Error: ${msg.message}`);
        break;
      case 'debug':
        logger.info(`ImmorTerm AI: [webview] ${msg.message}`);
        break;
      case 'paint-canary':
        this.handlePaintCanary(msg);
        break;
      case 'new-session':
        // User clicked + button in the sidebar
        this.startSession();
        break;
      case 'session-closed':
        // User closed a session from the sidebar — shelve (not destroy)
        if (typeof msg.sessionName === 'string') {
          this.shelveSession(msg.sessionName);
        }
        break;
      case 'session-switched':
        // User switched active AI tab — persist for focus restoration
        if (typeof msg.windowId === 'string') {
          setActiveTerminal('ai', msg.windowId);
          this.activeWindowId = msg.windowId;
          this.pushActiveBranchToWebview();
        }
        break;
      case 'get-shelved-sessions': {
        const shelved = getShelvedSessions();
        const hardEntries = shelved.map(e => ({
          windowId: e.window_id,
          name: e.display_name || e.name,
          sessionType: e.session_type || 'screen',
          shelvedAt: e.shelved_at || 0,
          claudeSessionId: e.claude_session_id || null,
          soft: false,
        }));
        // Soft-shelved entries are not in registry-shelved.json (their
        // entries stay in the live registry). Surface them here so the
        // reattach picker lists them with a "Sleeping" indicator.
        const softEntries = Array.from(this.softShelved.entries()).map(([wid, s]) => ({
          windowId: wid,
          name: s.displayName,
          sessionType: 'ai',
          shelvedAt: Math.floor(s.softShelvedAt / 1000),
          claudeSessionId: null,
          soft: true,
        }));
        this.view?.webview.postMessage({
          type: 'shelved-sessions',
          sessions: [...softEntries, ...hardEntries],
        });
        break;
      }
      case 'reattach-session': {
        const windowId = msg.windowId as string;
        if (windowId) {
          this.reattachSession(windowId).then(ok => {
            if (!ok) {
              this.view?.webview.postMessage({
                type: 'reattach-failed',
                windowId,
              });
            }
          });
        }
        break;
      }
      case 'delete-shelved-session': {
        const windowId = msg.windowId as string;
        if (windowId) {
          this.deleteShelvedSession(windowId).then(() => {
            // Refresh the shelved sessions modal
            const shelved = getShelvedSessions();
            this.view?.webview.postMessage({
              type: 'shelved-sessions',
              sessions: shelved.map(e => ({
                windowId: e.window_id,
                name: e.display_name || e.name,
                sessionType: e.session_type || 'screen',
                shelvedAt: e.shelved_at || 0,
                claudeSessionId: e.claude_session_id || null,
              })),
            });
          });
        }
        break;
      }
      case 'get-session-summary-by-id': {
        const windowId = msg.windowId as string;
        if (windowId) {
          this.showSessionSummary(windowId);
        }
        break;
      }
      case 'get-session-info': {
        const windowId = msg.windowId as string;
        const sessionName = msg.sessionName as string;
        if (windowId) {
          void this.collectSessionInfo(windowId, sessionName).then(info => {
            this.view?.webview.postMessage({ type: 'session-info-result', info });
          });
        }
        break;
      }
      case 'get-session-switch-summary': {
        // Lightweight (non-modal) fetch: powers the session-switch title
        // popover that briefly appears above the status bar when the user
        // switches terminals. Reuses the /sessions/context API but posts
        // only `title` + `at_a_glance` back to the webview and never
        // focuses the panel or opens the summary modal.
        const windowId = msg.windowId as string;
        if (windowId) {
          void this.fetchSessionSwitchSummary(windowId);
        }
        break;
      }
      case 'title-changed': {
        // Daemon detected an OSC 0/2 title change (e.g. Claude Code set the title)
        // NOTE: The webview now debounces (2s) and filters locked sessions before sending.
        // The webview also applies local sidebar/WASM updates, so no echo-back needed.
        const sessionName = msg.sessionName as string;
        const title = msg.title as string;
        const session = this.sessions.get(sessionName);
        if (!session) break;

        // Dedup: skip if identical to last received title
        if (title === this.lastTitleBySession.get(sessionName)) break;
        this.lastTitleBySession.set(sessionName, title);

        if (session.titleLocked) break; // webview filters these, but belt-and-suspenders

        logger.info(`ImmorTerm AI: title changed for '${sessionName}': '${title}'`);
        session.displayName = title;

        // Update registry with new display name
        if (session.windowId) {
          updateRegistryNameAndCommand(session.windowId, title);
        }
        // No echo-back postMessage — webview already updated locally
        break;
      }
      case 'rename-session': {
        // User double-clicked to rename a session tab → lock the title
        const sessionName = msg.sessionName as string;
        const displayName = msg.displayName as string;
        const session = this.sessions.get(sessionName);
        if (!session) break;

        logger.info(`ImmorTerm AI: user renamed '${sessionName}' → '${displayName}' (title locked)`);
        session.displayName = displayName;
        session.titleLocked = true;

        // Update registry: set display name + lock title
        if (session.windowId) {
          updateRegistryNameAndCommand(session.windowId, displayName);
          updateRegistryTitleLocked(session.windowId, true);
        }

        // Tell the webview to update sidebar + status bar
        this.view?.webview.postMessage({
          type: 'update-display-name',
          sessionName,
          displayName,
        });
        break;
      }
      case 'reorder-sessions': {
        const order = msg.order as string[];
        logger.info(`ImmorTerm AI: reorder-sessions received, ${order.length} sessions`);

        // Rebuild the Map in new order
        const newMap = new Map<string, typeof this.sessions extends Map<string, infer V> ? V : never>();
        for (const name of order) {
          const session = this.sessions.get(name);
          if (session) newMap.set(name, session);
        }
        // Keep any sessions not in the order list (safety)
        for (const [name, session] of this.sessions) {
          if (!newMap.has(name)) newMap.set(name, session);
        }
        this.sessions = newMap;

        // Persist to session-status.json (daemon-safe, not registry.json)
        const windowIds = order
          .map(name => this.sessions.get(name)?.windowId)
          .filter((id): id is string => !!id);
        updateRegistrySessionOrder(windowIds);
        break;
      }
      case 'share-session-context': {
        const sourceName = msg.sourceName as string;
        const targetName = msg.targetName as string;
        const sourceSession = this.sessions.get(sourceName);
        const targetSession = this.sessions.get(targetName);
        if (!sourceSession || !targetSession) break;

        const itemId = this.writeShareItem(targetName, {
          kind: 'session',
          source_immorterm_id: sourceSession.windowId,
          source_name: sourceSession.displayName || sourceName,
        });
        if (!itemId) { logger.warn(`ImmorTerm AI: share refused — target '${targetName}' has no id`); break; }
        this.view?.webview.postMessage({
          type: 'share-queued',
          targetSession: targetName,
          item: { id: itemId, kind: 'session', label: sourceSession.displayName || sourceName },
        });
        logger.info(`ImmorTerm AI: queued session share '${sourceName}' → '${targetName}' (${itemId})`);
        break;
      }
      case 'share-session-interactive': {
        const sourceName = msg.sourceName as string;
        const targetName = msg.targetName as string;
        const sourceSession = this.sessions.get(sourceName);
        const targetSession = this.sessions.get(targetName);
        if (!sourceSession || !targetSession) break;

        const itemId = this.writeShareItem(targetName, {
          kind: 'session',
          source_immorterm_id: sourceSession.windowId,
          source_name: sourceSession.displayName || sourceName,
          mode: 'interactive',
        });
        if (!itemId) break;

        // Pair the two daemons (live channel) — unchanged.
        this.view?.webview.postMessage({
          type: 'pair-sessions',
          sourceSessionName: sourceName,
          targetSessionName: targetName,
          sourceId: sourceSession.windowId,
          targetId: targetSession.windowId,
          sourceName: sourceSession.displayName || sourceName,
          targetName: targetSession.displayName || targetName,
        });
        this.view?.webview.postMessage({
          type: 'share-queued',
          targetSession: targetName,
          item: { id: itemId, kind: 'session', interactive: true, label: sourceSession.displayName || sourceName },
        });
        logger.info(`ImmorTerm AI: queued interactive share '${sourceName}' → '${targetName}' (${itemId})`);
        break;
      }
      case 'share-file-context': {
        // File dropped on the terminal → attached. One unified 'file' kind
        // (legacy 'file-explain'/'file-diff' still accepted). The hook injects
        // the path + an action menu; Claude decides what to do.
        const targetName = msg.targetName as string;
        const filePath = msg.filePath as string;
        const relPath = (msg.relPath as string) || '';
        const kind = (msg.kind as string) || 'file';
        if (!targetName || !filePath || !['file', 'file-explain', 'file-diff'].includes(kind)) break;
        const itemId = this.writeShareItem(targetName, { kind, file_path: filePath, rel_path: relPath });
        if (!itemId) { logger.warn(`ImmorTerm AI: file share refused — target '${targetName}' has no id`); break; }
        const base = (relPath || filePath).split('/').pop() || filePath;
        this.view?.webview.postMessage({
          type: 'share-queued',
          targetSession: targetName,
          item: { id: itemId, kind, label: base },
        });
        logger.info(`ImmorTerm AI: attached file '${base}' → '${targetName}' (${itemId})`);
        break;
      }
      case 'cancel-share-item': {
        // X on a specific pill — delete just that item. Untrack first so the
        // reconcile watcher does NOT treat it as a consumed task (no link).
        const targetSession = msg.targetSession as string;
        const itemId = msg.itemId as string;
        const target = this.sessions.get(targetSession);
        if (target?.windowId && itemId) {
          this.pendingTaskItems.delete(itemId);
          fs.unlink(path.join(this.shareQueueDir(target.windowId), `${itemId}.json`), () => {});
        }
        break;
      }
      case 'toggle-title-lock': {
        const sessionName = msg.sessionName as string;
        const session = this.sessions.get(sessionName);
        if (!session) break;

        const nowLocked = !session.titleLocked;
        session.titleLocked = nowLocked;

        if (session.windowId) {
          updateRegistryTitleLocked(session.windowId, nowLocked);
        }

        logger.info(`ImmorTerm AI: title ${nowLocked ? 'locked' : 'unlocked'} for '${sessionName}'`);

        this.view?.webview.postMessage({
          type: 'title-lock-changed',
          sessionName,
          locked: nowLocked,
        });
        break;
      }
      // ── Modal IPC handlers ──────────────────────────────────────
      case 'run-diagnostics':
        this.runDiagnostics();
        break;
      case 'get-service-status':
        this.sendServiceStatus();
        break;
      case 'retry-hub': {
        // Modal Retry button — re-run the sidecar bootstrap and post back
        // a `hub-status` event the modal renders inline. No matter the
        // outcome we always reply so the modal\u2019s loading state ends.
        (async () => {
          try {
            const { ensureHubRunning } = await import('./hub-sidecar');
            const status = await ensureHubRunning();
            this.view?.webview.postMessage({ type: 'hub-status', status });
          } catch (e) {
            const reason = e instanceof Error ? e.message : String(e);
            this.view?.webview.postMessage({
              type: 'hub-status',
              status: { running: false, reason: 'Retry failed.', details: reason },
            });
          }
        })();
        break;
      }
      case 'service-action': {
        const { serviceId, action } = msg as { serviceId: string; action: string; type: string };
        this.handleServiceAction(serviceId, action);
        break;
      }
      case 'service-toggle': {
        const { serviceId: svcId, enabled } = msg as { serviceId: string; enabled: boolean; type: string };
        this.handleServiceToggle(svcId, enabled);
        break;
      }
      case 'get-session-summary': {
        const sessionName = msg.sessionName as string;
        this.sendSessionSummary(sessionName);
        break;
      }
      case 'memory-search': {
        const { query, limit, offset, scopeAll } = msg as { query: string; limit: number; offset: number; scopeAll: boolean; type: string };
        this.searchMemories(query, limit, offset, scopeAll);
        break;
      }
      case 'get-license-status':
        this.sendLicenseStatus();
        break;
      case 'license-activate': {
        const { key } = msg as { key: string; type: string };
        this.handleLicenseActivate(key);
        break;
      }
      case 'license-deactivate':
        this.handleLicenseDeactivate();
        break;
      case 'license-validate':
        this.handleLicenseValidate();
        break;
      case 'open-external': {
        const { url } = msg as { url: string; type: string };
        vscode.env.openExternal(vscode.Uri.parse(url));
        break;
      }
      case 'explain-selection': {
        const { text, reqId, forceLLM } = msg as { text: string; reqId: number; forceLLM?: boolean; type: string };
        this.explainSelectionWithHaiku(text, reqId, !!forceLLM);
        break;
      }
      case 'fetch-url-preview': {
        const { url, requestId } = msg as { url: string; requestId: string; type: string };
        const send = (data: { title?: string; description?: string; image?: string; siteName?: string; video?: string; videoType?: string } | null) => {
          this.view?.webview.postMessage({ type: 'url-preview-result', requestId, data });
        };
        try {
          const parsed = new URL(url);
          const client = parsed.protocol === 'http:' ? http : https;
          let redirects = 0;
          const fetchOnce = (u: string) => {
            const req = client.get(u, {
              headers: { 'User-Agent': 'Mozilla/5.0 (ImmorTerm link preview)', 'Accept': 'text/html' },
              timeout: 3000,
            }, (res) => {
              const status = res.statusCode ?? 0;
              const loc = res.headers.location;
              if (status >= 300 && status < 400 && loc && redirects < 3) {
                redirects++;
                res.resume();
                fetchOnce(new URL(loc, u).toString());
                return;
              }
              if (status !== 200) { res.resume(); return send(null); }
              const ctype = String(res.headers['content-type'] ?? '');
              if (!ctype.includes('text/html') && !ctype.includes('application/xhtml')) { res.resume(); return send(null); }
              const chunks: Buffer[] = [];
              let total = 0;
              let finished = false;
              const finish = () => {
                if (finished) return;
                finished = true;
                const html = Buffer.concat(chunks).toString('utf8');
                const head = html.slice(0, html.toLowerCase().indexOf('</head>') + 7) || html.slice(0, 50000);
                const get = (prop: string): string | undefined => {
                  const re = new RegExp(`<meta[^>]+(?:property|name)=["']${prop}["'][^>]+content=["']([^"']+)["']`, 'i');
                  const alt = new RegExp(`<meta[^>]+content=["']([^"']+)["'][^>]+(?:property|name)=["']${prop}["']`, 'i');
                  const m = head.match(re) || head.match(alt);
                  return m ? decodeHtmlEntities(m[1]) : undefined;
                };
                const title = get('og:title') ?? (head.match(/<title[^>]*>([^<]+)<\/title>/i)?.[1]?.trim());
                const description = get('og:description') ?? get('description');
                const image = get('og:image');
                const siteName = get('og:site_name');
                const absImage = image ? new URL(image, u).toString() : undefined;
                const video = get('og:video:secure_url') ?? get('og:video:url') ?? get('og:video');
                const videoType = get('og:video:type');
                const absVideo = video ? new URL(video, u).toString() : undefined;
                // Only surface video if it's a directly-playable format (mp4/webm/ogg). Flash/iframe
                // embeds use `og:video:type: text/html` and won't play in a <video> element.
                const playable = !videoType || /^video\/(mp4|webm|ogg)/i.test(videoType);
                send({
                  title: title ? decodeHtmlEntities(title) : undefined,
                  description,
                  image: absImage,
                  siteName,
                  video: playable ? absVideo : undefined,
                  videoType: playable ? videoType : undefined,
                });
              };
              res.on('data', (c: Buffer) => {
                total += c.length;
                chunks.push(c);
                if (total > 131072) { res.destroy(); finish(); }
              });
              res.on('end', finish);
              res.on('error', () => send(null));
            });
            req.on('timeout', () => { req.destroy(); send(null); });
            req.on('error', () => send(null));
          };
          fetchOnce(parsed.toString());
        } catch { send(null); }
        break;
      }
      case 'check-link-exists': {
        const { path: p, requestId, cwd, line: targetLine } = msg as { path: string; requestId: string; cwd?: string; line?: number | null; type: string };
        resolveLinkPathAsync(p, cwd).then((resolved) => {
        fs.stat(resolved, (err, st) => {
          const exists = !err && !!st;
          const isDir = exists && st!.isDirectory();
          const uri = vscode.Uri.file(resolved);
          const inWorkspace = exists ? !!vscode.workspace.getWorkspaceFolder(uri) : false;
          const fileExt = isDir ? undefined : path.extname(resolved).toLowerCase().replace(/^\./, '');
          const IMAGE_EXTS = new Set(['png','jpg','jpeg','gif','webp','svg','ico','bmp']);
          const VIDEO_EXTS = new Set(['mp4','webm','mov','mkv','m4v','ogv']);
          const MD_EXTS = new Set(['md','markdown']);
          const HTML_EXTS = new Set(['html','htm']);
          const send = (preview?: string, previewStartLine?: number, imageDataUrl?: string, videoUri?: string, markdownRaw?: string, markdownFences?: Array<{lang: string; tokens: any}>, fileDir?: string, htmlRaw?: string, absolutePath?: string, htmlWebviewUri?: string) => {
            this.view?.webview.postMessage({
              type: 'link-exists-result',
              requestId,
              exists,
              kind: isDir ? 'dir' : 'file',
              inWorkspace,
              preview,
              previewStartLine,
              previewTokens: null,
              ext: fileExt,
              imageDataUrl,
              videoUri,
              markdownRaw,
              markdownFences,
              fileDir,
              htmlRaw,
              absolutePath,
              htmlWebviewUri,
            });
          };
          // Follow-up post: deliver syntax-highlight tokens for a window around the
          // target line. Webview patches rows in place once this arrives.
          const sendPreviewTokens = async (fullText: string) => {
            try {
              const win = await tokenizeWindow(fullText, fileExt, targetLine ?? undefined);
              if (!win) return;
              this.view?.webview.postMessage({
                type: 'preview-tokens',
                requestId,
                tokens: win.tokens,
                tokenStartLine: win.startLine,
              });
            } catch (err) {
              logger.warn('[preview-tokens] failed: ' + String(err));
            }
          };
          if (exists && !isDir && fileExt && VIDEO_EXTS.has(fileExt)) {
            try {
              const uri = this.view?.webview.asWebviewUri(vscode.Uri.file(resolved)).toString();
              send(undefined, undefined, undefined, uri);
            } catch { send(); }
          } else if (exists && !isDir && fileExt && MD_EXTS.has(fileExt) && st!.size <= 262_144) {
            fs.readFile(resolved, 'utf8', async (rerr, raw) => {
              if (rerr) return send();
              const fenceRe = /```([a-zA-Z0-9_+-]*)\n([\s\S]*?)```/g;
              const fences: Array<{lang: string; tokens: any}> = [];
              let fm: RegExpExecArray | null;
              while ((fm = fenceRe.exec(raw)) !== null) {
                const lang = (fm[1] || '').toLowerCase();
                const tokens = await tokenizeSnippet(fm[2], lang || undefined);
                fences.push({ lang, tokens });
              }
              send(undefined, undefined, undefined, undefined, raw, fences, path.dirname(resolved));
            });
          } else if (exists && !isDir && fileExt && HTML_EXTS.has(fileExt) && st!.size <= 524_288) {
            fs.readFile(resolved, 'utf8', (rerr, raw) => {
              if (rerr) return send();
              // asWebviewUri lets the iframe load with a real origin so
              // relative <link>/<img> paths resolve via VS Code's local
              // resource server (localResourceRoots = '/'). Without this we
              // fall back to srcdoc — fine for self-contained HTML, broken
              // for anything with external CSS.
              const webviewUri = (() => {
                try { return this.view?.webview.asWebviewUri(vscode.Uri.file(resolved)).toString(); }
                catch { return undefined; }
              })();
              send(undefined, undefined, undefined, undefined, undefined, undefined, path.dirname(resolved), raw, resolved, webviewUri);
            });
          } else if (exists && !isDir && fileExt && IMAGE_EXTS.has(fileExt) && st!.size <= 50_000_000) {
            // Zero-copy webview URI works for any size (localResourceRoots = '/').
            // Base64 fallback only triggers if URI creation fails; cap it at 5MB
            // to avoid postMessage bloat.
            try {
              const imgUri = this.view?.webview.asWebviewUri(vscode.Uri.file(resolved)).toString();
              if (imgUri) { send(undefined, undefined, imgUri); return; }
            } catch { /* fall through */ }
            if (st!.size > 5_000_000) { send(); return; }
            fs.readFile(resolved, (rerr, data) => {
              if (rerr) return send();
              const mime = fileExt === 'svg' ? 'image/svg+xml' : `image/${fileExt === 'jpg' ? 'jpeg' : fileExt}`;
              send(undefined, undefined, `data:${mime};base64,${data.toString('base64')}`);
            });
          } else if (exists && !isDir && st!.size <= 1_500_000) {
            // Read the full file. Webview virtualizes scroll so DOM node count
            // stays bounded regardless of line count, and the user can scroll the
            // whole file + lazy-load tokens. Cap raised from 512KB so big source
            // files (e.g. our ~600KB gpu-terminal.html) ship in full instead of
            // falling through to the windowed branch.
            // Raw content ships immediately; tokens follow asynchronously as a
            // `preview-tokens` message so the modal opens without waiting on Shiki.
            fs.readFile(resolved, (rerr, data) => {
              if (rerr) return send();
              const head = data.subarray(0, Math.min(4096, data.length));
              if (head.includes(0)) return send();
              const text = data.toString('utf8');
              rememberPreviewText(requestId, text, fileExt);
              send(text, 1);
              sendPreviewTokens(text);
            });
          } else if (exists && !isDir) {
            // Too large to ship whole — stream a window of lines AROUND the target
            // line. The old code read only the first 64KB, so a target line past
            // that (common in big files) produced an empty preview.
            const target = (targetLine && targetLine > 0) ? targetLine : 1;
            const WINDOW = 120;
            const startLine = Math.max(1, target - WINDOW);
            const endLine = target + WINDOW;
            const stream = fs.createReadStream(resolved, { encoding: 'utf8' });
            const rl = readline.createInterface({ input: stream, crlfDelay: Infinity });
            let ln = 0;
            let binary = false;
            const out: string[] = [];
            rl.on('line', (line) => {
              ln++;
              if (ln === 1 && line.includes('\0')) { binary = true; rl.close(); return; }
              if (ln >= startLine && ln <= endLine) out.push(line);
              if (ln > endLine) rl.close();
            });
            rl.on('close', () => {
              try { stream.destroy(); } catch { /* ignore */ }
              if (binary || out.length === 0) { send(); return; }
              const snippet = out.join('\n');
              send(snippet, startLine);
              (async () => {
                try {
                  const tokens = await tokenizeSnippet(snippet, fileExt);
                  if (!tokens) return;
                  this.view?.webview.postMessage({
                    type: 'preview-tokens',
                    requestId,
                    tokens,
                    tokenStartLine: startLine,
                  });
                } catch (err) {
                  logger.warn('[preview-tokens snippet] failed: ' + String(err));
                }
              })();
            });
            rl.on('error', () => { try { stream.destroy(); } catch { /* ignore */ } send(); });
          } else if (exists && isDir) {
            // Directory: no preview body, but ship the RESOLVED absolute path so
            // the webview's "Browse here" tree can fetch /api/ls with an absolute
            // path. The raw link text is often relative (e.g. a path selected out
            // of a tool-call line), which /api/ls can't resolve on its own.
            send(undefined, undefined, undefined, undefined, undefined, undefined, undefined, undefined, resolved);
          } else {
            send();
          }
        });
        });
        break;
      }
      case 'list-dir': {
        // Directory listing for the in-tooltip "Browse here" tree. VS Code's
        // webview can't reach the hub's /api/ls (wrong origin), so the host
        // reads the dir directly. `path` is expected to be absolute (the webview
        // passes the host-resolved absolutePath from check-link-exists).
        const { path: dirPath, requestId, cwd } = msg as { path: string; requestId: string; cwd?: string; type: string };
        resolveLinkPathAsync(dirPath, cwd).then((resolved) => {
          fs.readdir(resolved, { withFileTypes: true }, (err, dirents) => {
            if (err) {
              this.view?.webview.postMessage({ type: 'list-dir-result', requestId, entries: [], error: String(err.message || err) });
              return;
            }
            const entries = dirents.map((de) => ({
              name: de.name,
              kind: de.isDirectory() ? 'dir' : (de.isSymbolicLink() ? 'link' : 'file'),
            }));
            entries.sort((a, b) => {
              if ((a.kind === 'dir') !== (b.kind === 'dir')) return a.kind === 'dir' ? -1 : 1;
              return a.name.localeCompare(b.name);
            });
            this.view?.webview.postMessage({ type: 'list-dir-result', requestId, entries });
          });
        });
        break;
      }
      case 'resolve-ws-port': {
        // Re-resolve a session's CURRENT WebSocket port from the live .ws file.
        // The webview caches wsPort at addSession() time; after a daemon respawn
        // (e.g. reboot) that port is dead but the daemon is alive on a NEW port.
        // The webview is sandboxed and can't read SOCKET_DIR, so it asks the host.
        // Best-effort + non-throwing: reply with wsPort:null if it can't resolve yet.
        const { sessionName } = msg as { sessionName: string; type: string };
        let wsPort: number | null = null;
        try {
          const daemonPid = findDaemonPidFromWsFile(sessionName);
          wsPort = daemonPid ? findWsPort(sessionName, daemonPid) : findWsPort(sessionName);
        } catch {
          wsPort = null;
        }
        this.view?.webview.postMessage({ type: 'ws-port-resolved', sessionName, wsPort });
        break;
      }
      case 'resolve-claude-image': {
        // `[Image #N]` paste preview. Resolve N → an image-cache PNG. The
        // webview can't always learn the active session's Claude UUID (the
        // jsonl-tail tracker is flaky), so scan ~/.claude/image-cache/*/<N>.png
        // and pick the newest — preferring the known UUID's dir when supplied.
        const { requestId, n, uuid } = msg as { requestId: string; n: number; uuid?: string; type: string };
        const cacheRoot = path.join(os.homedir(), '.claude', 'image-cache');
        const fname = String(n) + '.png';
        const reply = (resolvedPath?: string) => {
          let imageDataUrl: string | undefined;
          if (resolvedPath) {
            try { imageDataUrl = this.view?.webview.asWebviewUri(vscode.Uri.file(resolvedPath)).toString(); } catch { /* ignore */ }
          }
          this.view?.webview.postMessage({ type: 'claude-image-result', requestId, exists: !!imageDataUrl, imageDataUrl });
        };
        const preferred = uuid ? path.join(cacheRoot, uuid, fname) : null;
        if (preferred && fs.existsSync(preferred)) { reply(preferred); break; }
        fs.readdir(cacheRoot, { withFileTypes: true }, (err, dirents) => {
          if (err) { reply(); return; }
          let best: { p: string; m: number } | null = null;
          for (const de of dirents) {
            if (!de.isDirectory()) continue;
            const cand = path.join(cacheRoot, de.name, fname);
            try {
              const s = fs.statSync(cand);
              if (!best || s.mtimeMs > best.m) best = { p: cand, m: s.mtimeMs };
            } catch { /* not in this dir */ }
          }
          reply(best ? best.p : undefined);
        });
        break;
      }
      case 'request-preview-range': {
        // Webview asks for Shiki tokens over a line range of a previously
        // opened file preview. Served from previewTextCache keyed by the
        // original check-link-exists requestId; no re-read, no stat.
        const { requestId, startLine: reqStart, endLine: reqEnd } = msg as { requestId: string; startLine: number; endLine: number; type: string };
        const cached = previewTextCache.get(requestId);
        if (!cached || !cached.ext) break;
        const lines = cached.text.split('\n');
        const totalLines = lines.length;
        const s = Math.max(1, reqStart | 0);
        const e = Math.min(totalLines, reqEnd | 0);
        if (e < s) break;
        const chunk = lines.slice(s - 1, e).join('\n');
        const rangeLang = SHIKI_EXT_TO_LANG[cached.ext];
        const rangePrefix = rangeLang && STATEFUL_LANGS.has(rangeLang) && s > 1
          ? lines.slice(0, s - 1).join('\n')
          : undefined;
        (async () => {
          try {
            const tokens = await tokenizeSnippet(chunk, cached.ext, rangePrefix);
            if (!tokens) return;
            this.view?.webview.postMessage({
              type: 'preview-tokens',
              requestId,
              tokens,
              tokenStartLine: s,
            });
          } catch (err) {
            logger.warn('[preview-range] tokenize failed: ' + String(err));
          }
        })();
        break;
      }
      case 'open-file-link': {
        const { path: p, line, col, cwd } = msg as { path: string; line?: number; col?: number; cwd?: string; type: string };
        resolveLinkPathAsync(p, cwd).then((resolved) => {
        logger.info(`open-file-link: ${p} → ${resolved} line=${line} col=${col}`);
        fs.stat(resolved, (err, st) => {
          if (err) {
            logger.warn(`open-file-link stat failed: ${err.message}`);
            vscode.window.showWarningMessage(`Path not found: ${resolved}`);
            return;
          }
          const uri = vscode.Uri.file(resolved);
          if (st.isDirectory()) {
            const inWorkspace = vscode.workspace.getWorkspaceFolder(uri);
            if (inWorkspace) {
              vscode.commands.executeCommand('revealInExplorer', uri).then(undefined, (e) => {
                logger.warn(`revealInExplorer failed: ${e?.message ?? e}`);
              });
            } else {
              // Directory outside workspace — open in OS file manager (Finder/Explorer).
              vscode.env.openExternal(uri).then(undefined, (e) => {
                logger.warn(`openExternal(dir) failed: ${e?.message ?? e}`);
              });
            }
            return;
          }
          const hasLine = line !== undefined && line !== null;
          const l = hasLine ? Math.max(0, (line as number) - 1) : 0;
          const c = Math.max(0, ((col ?? 1) as number) - 1);
          const selection = hasLine ? new vscode.Range(l, c, l, c) : undefined;
          vscode.commands.executeCommand('vscode.open', uri, { selection, preview: true }).then(undefined, (e) => {
            logger.warn(`vscode.open failed: ${e?.message ?? e}`);
            vscode.window.showTextDocument(uri, { selection, preview: true }).then(undefined, (e2) => {
              logger.warn(`showTextDocument fallback failed: ${e2?.message ?? e2}`);
            });
          });
        });
        });
        break;
      }
      case 'save-claude-image': {
        // Save a [Image #N] paste cache to a user-chosen location.
        // path is `~/.claude/image-cache/<uuid>/<N>.png`; n is the original
        // counter so we can name the default file `claude-image-<N>.png`.
        const { path: srcPath, n: imgN } = msg as { path: string; n: number; type: string };
        resolveLinkPathAsync(srcPath).then(async (resolved) => {
          try { await fs.promises.stat(resolved); }
          catch {
            vscode.window.showWarningMessage(`Pasted image not found: ${resolved}`);
            return;
          }
          const defaultUri = vscode.Uri.file(
            path.join(os.homedir(), 'Downloads', `claude-image-${imgN}.png`),
          );
          const target = await vscode.window.showSaveDialog({
            defaultUri,
            filters: { Images: ['png'] },
            saveLabel: 'Save image',
          });
          if (!target) return;
          try {
            await fs.promises.copyFile(resolved, target.fsPath);
          } catch (err) {
            vscode.window.showErrorMessage(`Save failed: ${(err as Error).message}`);
          }
        });
        break;
      }
      case 'reveal-file-link': {
        const { path: p, cwd } = msg as { path: string; cwd?: string; type: string };
        resolveLinkPathAsync(p, cwd).then((resolved) => {
          const uri = vscode.Uri.file(resolved);
          vscode.commands.executeCommand('revealFileInOS', uri).then(undefined, (e) => {
            logger.warn(`revealFileInOS failed: ${e?.message ?? e}`);
            vscode.env.openExternal(uri);
          });
        });
        break;
      }
      case 'lookup-task': {
        const { subject, requestId, immortermId } = msg as { subject: string; requestId: string; immortermId?: string; type: string };
        this.lookupTask(subject, requestId, immortermId);
        break;
      }
      case 'lookup-all-tasks': {
        const { requestId: allTasksReqId, immortermId: allTasksImId } = msg as { requestId: string; immortermId?: string; type: string };
        this.lookupAllTasks(allTasksReqId, allTasksImId);
        break;
      }
      case 'add-immorterm-task': {
        const { subject: taskSubject, description: taskDesc, status: taskStatus } = msg as {
          subject: string; description: string; status: string; type: string;
        };
        this.addImmorTermTask(taskSubject, taskDesc, taskStatus);
        break;
      }
      case 'get-session-logs':
        this.sendSessionLogs();
        break;
      case 'open-session-log': {
        const { sessionName: logSession, logType } = msg as { sessionName: string; logType: string; type: string };
        this.openSessionLog(logSession, logType);
        break;
      }
      case 'wizard-check': {
        const { step } = msg as { step: string; type: string };
        this.handleWizardCheck(step);
        break;
      }
      case 'wizard-action': {
        const { action: wizAction } = msg as { action: string; type: string };
        this.handleWizardAction(wizAction);
        break;
      }
      case 'get-insights': {
        const sessionName = msg.sessionName as string | undefined;
        this.sendInsights(sessionName);
        break;
      }
      case 'get-preferences': {
        const prefs = getAppearance();
        this.view?.webview.postMessage({
          type: 'preferences',
          borderEnabled: prefs.borderEnabled,
          borderOpacity: prefs.borderOpacity,
          statusBar: prefs.statusBarEnabled,
          statusBarMode: prefs.statusBarMode,
          animations: prefs.statusBarAnimations,
          expressionEffects: prefs.expressionEffects,
          celebrations: prefs.celebrations,
          dangerEffects: prefs.dangerEffects,
          textAnimations: prefs.textAnimations,
          // Raw (unmerged) modes — same no-clobber policy as the boot push:
          // undefined = no stored choice, webview state wins.
          sidebarMode: getRawAppearance().sidebarMode,
          tasksMode: getRawAppearance().tasksMode,
          workshopsMode: prefs.workshopsMode,
          railsEnabled: prefs.railsEnabled,
          railLayout: prefs.railLayout,
          viewModes: prefs.viewModes,
          backgroundControlMode: prefs.backgroundControlMode,
        });
        break;
      }
      case 'save-preference': {
        const { key, value } = msg as { key: string; value: unknown; type: string };
        // Namespaced passthrough for views added after S2 (plans/projects/…):
        // 'viewMode.<id>' keys land in the appearance config's viewModes
        // record with zero per-view host edits. The four original views keep
        // their legacy top-level keys below.
        if (key.startsWith('viewMode.')) {
          try {
            const viewModes = { ...getAppearance().viewModes, [key.slice('viewMode.'.length)]: String(value) };
            updateAppearance({ viewModes });
          } catch (err) {
            logger.warn(`ImmorTerm AI: failed to save preference '${key}': ${err}`);
          }
          break;
        }
        const appearanceKeyMap: Record<string, keyof AppearanceConfig> = {
          borderEnabled: 'borderEnabled',
          borderOpacity: 'borderOpacity',
          statusBarEnabled: 'statusBarEnabled',
          statusBarAnimations: 'statusBarAnimations',
          statusBarMode: 'statusBarMode',
          expressionEffects: 'expressionEffects',
          celebrations: 'celebrations',
          dangerEffects: 'dangerEffects',
          textAnimations: 'textAnimations',
          sidebarMode: 'sidebarMode',
          // fileBrowserMode is intentionally absent: the file browser
          // persists per-project via the hub (PUT /api/v1/config/project),
          // not in the global appearance config.
          tasksMode: 'tasksMode',
          workshopsMode: 'workshopsMode',
          railsEnabled: 'railsEnabled',
          railLayout: 'railLayout',
          backgroundControlMode: 'backgroundControlMode',
        };
        const appearanceKey = appearanceKeyMap[key];
        if (appearanceKey) {
          try {
            updateAppearance({ [appearanceKey]: value } as Partial<AppearanceConfig>);
          } catch (err) {
            logger.warn(`ImmorTerm AI: failed to save preference '${key}': ${err}`);
          }
        }
        break;
      }
      case 'theme-changed': {
        const { themeName, sessionName: changedSession } = msg as { themeName: string; sessionName?: string; type: string };
        logger.info(`ImmorTerm AI: theme changed to '${themeName}' (session: ${changedSession || 'unknown'})`);

        // Per-terminal: save to the active session's registry entry
        const themeSession = changedSession ? this.sessions.get(changedSession) : undefined;
        if (themeSession?.windowId) {
          updateRegistryTheme(themeSession.windowId, themeName);
        }

        // Per-project: save as project default so new terminals inherit it
        if (this.projectPath) {
          const projectId = getStableProjectId(this.projectPath);
          setTheme(this.projectPath, themeName, projectId);
        }

        // Sync to VS Code workspace settings (so applyTheme QuickPick shows correct current)
        vscode.workspace.getConfiguration('immorterm').update(
          'statusBarTheme', themeName, vscode.ConfigurationTarget.Workspace,
        );

        // Broadcast: update all other AI sessions' registry entries too
        for (const [name, s] of this.sessions) {
          if (name !== changedSession && s.windowId) {
            updateRegistryTheme(s.windowId, themeName);
          }
        }

        // Tell the webview to apply the theme across all AI session tabs
        this.view?.webview.postMessage({ type: 'apply-theme', themeName });

        // Apply theme to all regular immorterm (screen) terminals
        applyThemeToAllScreenSessions(themeName).catch(err =>
          logger.debug(`ImmorTerm AI: failed to apply theme to screen sessions: ${err}`),
        );

        // Update screenrc on disk so NEW screen sessions inherit the theme
        if (this.projectPath) {
          try {
            const screenrcPath = getProjectScreenrcPath(this.projectPath);
            const existing = fs.readFileSync(screenrcPath, 'utf-8');
            const theme = getThemeObject(themeName);
            const themedHardstatus = `hardstatus alwayslastline ${generateHardstatus(theme)}`;
            const updated = existing.replace(/^hardstatus alwayslastline .+$/m, themedHardstatus);
            fs.writeFileSync(screenrcPath, updated, 'utf-8');
            logger.info(`Updated screenrc with theme '${themeName}'`);
          } catch (err) {
            logger.debug(`Failed to update screenrc: ${err}`);
          }
        }
        break;
      }
      case 'clipboard-image': {
        // Webview detected an image in the clipboard — save to temp file and
        // send the path back so it can be inserted into the terminal as input.
        const { data, mimeType } = msg as { data: string; mimeType: string; type: string };
        const ext = mimeType === 'image/jpeg' ? 'jpg' : 'png';
        const tmpFile = path.join(os.tmpdir(), `immorterm-paste-${Date.now()}.${ext}`);
        try {
          fs.writeFileSync(tmpFile, Buffer.from(data, 'base64'));
          this.view?.webview.postMessage({ type: 'clipboard-image-saved', filePath: tmpFile });
          logger.info(`ImmorTerm AI: clipboard image saved to ${tmpFile}`);
        } catch (err) {
          logger.error(`ImmorTerm AI: failed to save clipboard image: ${err}`);
        }
        break;
      }
      case 'set-speak-mode': {
        // Speak Mode write — project-level default OR per-session override.
        // scope: 'project' writes .immorterm/config.json (inherits for future sessions).
        // scope: 'session' writes the session's registry entry (requires daemon API — Task #7).
        const { mode, scope, sessionName } = msg as {
          mode: string; scope: 'project' | 'session'; sessionName?: string; type: string;
        };
        if (typeof mode !== 'string' || !(mode in CHARACTER_DEFS)) {
          logger.warn(`ImmorTerm AI: rejected unknown speakMode '${mode}'`);
          break;
        }
        if (scope === 'project' && this.projectPath) {
          const projectId = getStableProjectId(this.projectPath);
          setSpeakMode(this.projectPath, mode, projectId);
          logger.info(`ImmorTerm AI: project speakMode set to '${mode}'`);
          // Broadcast the new project default so all open terminals refresh their UI
          this.view?.webview.postMessage({ type: 'speak-mode-updated', projectSpeakMode: mode });
        } else if (scope === 'session') {
          // Per-session override persists in session-status.json (daemon-safe;
          // the Rust daemon strips unknown fields from registry.json).
          const target = sessionName ? this.sessions.get(sessionName) : undefined;
          if (target) {
            // If transitioning AWAY from a non-default persona back to
            // default/cleared, drop a one-shot reset marker so the next
            // prompt nudges the model out of the accumulated style.
            // LLMs tend to keep the persona for a few turns otherwise.
            const priorMode = target.windowId ? getSessionSpeakMode(target.windowId) : undefined;
            const wasActivePersona = priorMode && priorMode !== 'default';
            target.speakMode = mode === 'default' ? undefined : mode;
            if (target.windowId) {
              updateSessionSpeakMode(target.windowId, mode);
              if (wasActivePersona && (mode === 'default' || mode === '')) {
                markSpeakModeReset(target.windowId);
              }
            }
            logger.info(`ImmorTerm AI: session '${sessionName}' speakMode override set to '${mode}'`);
            this.view?.webview.postMessage({
              type: 'speak-mode-updated', sessionName, sessionSpeakMode: mode,
            });
          }
        }
        break;
      }
      // ── Pomodoro file I/O (thin adapter — all logic in webview) ──
      case 'pomodoro-save-state': {
        if (!this.projectPath) break;
        const pomDir = path.join(this.projectPath, '.immorterm');
        const pomFile = path.join(pomDir, 'pomodoro.json');
        try {
          fs.mkdirSync(pomDir, { recursive: true });
          fs.writeFileSync(pomFile, JSON.stringify(msg.data, null, 2));
        } catch (err) {
          logger.warn(`ImmorTerm AI: failed to save pomodoro state: ${err}`);
        }
        break;
      }
      case 'get-pomodoro-state': {
        if (!this.projectPath) break;
        const pomFile = path.join(this.projectPath, '.immorterm', 'pomodoro.json');
        try {
          const raw = fs.readFileSync(pomFile, 'utf-8');
          this.view?.webview.postMessage({ type: 'pomodoro-load-state', data: JSON.parse(raw) });
        } catch {
          // No saved state yet — send null so webview uses defaults
          this.view?.webview.postMessage({ type: 'pomodoro-load-state', data: null });
        }
        break;
      }
      // ── Tasks ──
      case 'get-tasks': {
        this.sendTasksToWebview();
        break;
      }
      case 'create-task': {
        if (!this.taskStorage) break;
        const originContext = this.getTaskOriginContext();
        const newTask = this.taskStorage.create(
          msg.title as string,
          (msg.taskType as string as 'bug' | 'feature' | 'investigate' | 'other') || 'other',
          (msg.lane as string as 'now' | 'next' | 'later') || 'next',
          originContext,
          (msg.description as string) || undefined,
        );

        if (newTask) {
          this.knownTaskIds.add(newTask.id);
          this.resolveTaskMemorySummaryId(newTask.id).catch(() => { /* best effort */ });
        }

        // AI enrichment: if title-only (no description), call Haiku to enrich
        if (newTask && !msg.description && msg.aiEnrich !== false) {
          this.aiEnrichTask(newTask);
        }
        break;
      }
      case 'update-task': {
        if (!this.taskStorage) break;
        const fields: Record<string, unknown> = {};
        if (msg.title !== undefined) fields.title = msg.title;
        if (msg.description !== undefined) fields.description = msg.description;
        if (msg.taskType !== undefined) fields.type = msg.taskType;
        if (msg.lane !== undefined) fields.lane = msg.lane;
        if (msg.status !== undefined) fields.status = msg.status;
        // Check if transitioning to done — trigger animation and pre-set prev status
        // so sendTasksToWebview won't double-fire
        if (msg.status === 'done') {
          this.view?.webview.postMessage({ type: 'task-done-animate', taskId: msg.taskId });
          this._prevTaskStatuses.set(msg.taskId as string, 'done');
        }
        this.taskStorage.update(msg.taskId as string, fields as any);
        break;
      }
      case 'delete-task': {
        if (!this.taskStorage) break;
        this.taskStorage.delete(msg.taskId as string);
        break;
      }
      case 'reorder-tasks': {
        if (!this.taskStorage) break;
        this.taskStorage.reorder(msg.taskIds as string[]);
        break;
      }
      case 'unlink-task-session': {
        if (!this.taskStorage) break;
        this.taskStorage.unlinkSession(msg.taskId as string, msg.immortermId as string);
        break;
      }
      case 'drop-task-on-session': {
        if (!this.taskStorage) break;
        const taskId = msg.taskId as string;
        const targetName = msg.targetName as string;
        const targetSession = this.sessions.get(targetName);
        if (!targetSession) break;

        const task = this.taskStorage.getById(taskId);
        if (!task) break;

        // Queue the task as a share item. Linking is deferred: it happens only
        // when the hook CONSUMES the item (tracked in pendingTaskItems), so an
        // X-cancel before the next prompt commits nothing.
        const itemId = this.writeShareItem(targetName, {
          kind: 'task',
          task_id: task.id,
          task_title: task.title,
          task_description: task.description,
          task_type: task.type,
          context: task.context,
          linked_sessions: task.linkedSessions.map(s => ({ immorterm_id: s.immortermId, session_name: s.sessionName })),
        });
        if (!itemId) { logger.warn(`ImmorTerm AI: task share refused — target '${targetName}' has no id`); break; }
        this.pendingTaskItems.set(itemId, {
          taskId,
          targetWindowId: targetSession.windowId,
          displayName: targetSession.displayName || targetName,
        });
        this.view?.webview.postMessage({
          type: 'share-queued',
          targetSession: targetName,
          item: { id: itemId, kind: 'task', label: task.title },
        });
        logger.info(`ImmorTerm AI: queued task '${task.title}' → '${targetName}' (${itemId})`);
        break;
      }
      case 'switch-to-task-session': {
        const targetImmorTermId = msg.immortermId as string;
        // Find session by windowId and switch to it
        for (const [name, session] of this.sessions) {
          if (session.windowId === targetImmorTermId) {
            this.view?.webview.postMessage({ type: 'focus-session', sessionName: name });
            break;
          }
        }
        break;
      }
      case 'create-task-from-selection': {
        if (!this.taskStorage) break;
        const selectedText = (msg.selectedText as string || '').trim();
        if (!selectedText) break;

        // Use first line (capped at 80 chars) as title, full text as context
        const firstLine = selectedText.split('\n')[0].slice(0, 80);
        const originContext = this.getTaskOriginContext();
        originContext.selectedText = selectedText;
        originContext.sourceImmorTermId = (msg.windowId as string) || originContext.sourceImmorTermId;

        const task = this.taskStorage.create(firstLine, 'investigate', 'next', originContext);

        if (task) {
          this.knownTaskIds.add(task.id);
          this.resolveTaskMemorySummaryId(task.id).catch(() => { /* best effort */ });
          this.aiEnrichTask(task);
        }

        // Open quick-add in sidebar so user can refine title/type
        this.sendTasksToWebview();
        break;
      }
    }
  }

  private _prevTaskStatuses = new Map<string, string>();

  /**
   * Called when TaskStorage detects an external write (e.g., MCP tool from the daemon).
   * Detects newly added tasks that bypassed the UI-enrichment path and runs AI enrichment
   * on them so MCP-created tasks get descriptions/type/lane filled in.
   */
  private onExternalTaskChange(): void {
    if (!this.taskStorage) return;
    for (const task of this.taskStorage.list()) {
      if (this.knownTaskIds.has(task.id)) continue;
      this.knownTaskIds.add(task.id);
      this.resolveTaskMemorySummaryId(task.id).catch(() => { /* best effort */ });
      if (!task.description) {
        logger.info(`ImmorTerm AI: MCP-created task detected, enriching: ${task.title}`);
        this.aiEnrichTask(task).catch(err => logger.warn('aiEnrichTask failed:', err));
      }
    }
  }

  private sendTasksToWebview(): void {
    if (!this.taskStorage || !this.view) return;
    const tasks = this.taskStorage.toJSON().tasks;

    // Detect tasks that just transitioned to 'done' — send animation BEFORE re-render
    for (const task of tasks) {
      const prev = this._prevTaskStatuses.get(task.id);
      if (prev && prev !== 'done' && task.status === 'done') {
        this.view.webview.postMessage({ type: 'task-done-animate', taskId: task.id });
      }
    }

    this.view.webview.postMessage({ type: 'tasks-load', tasks });

    // Snapshot current states
    this._prevTaskStatuses.clear();
    for (const task of tasks) {
      this._prevTaskStatuses.set(task.id, task.status);
    }
  }

  /**
   * Build origin context for a new task: immorterm ID, Claude Code session UUID,
   * and OpenMemory session UUID (from claude-env files).
   */
  private getTaskOriginContext(): TaskContext {
    const activeSession = [...this.sessions.values()].find(s => s.windowId);
    const ctx: TaskContext = {
      cwd: this.projectPath || process.cwd(),
      sourceImmorTermId: activeSession?.windowId,
    };

    // Resolve Claude Code session UUID from claude-env files.
    // The SessionStart hook writes files named <session_uuid>.env containing IMMORTERM_ID=<windowId>.
    // Find the most recent env file matching this immorterm_id.
    if (ctx.sourceImmorTermId) {
      try {
        const envDir = path.join(os.homedir(), '.immorterm', 'claude-env');
        const files = fs.readdirSync(envDir).filter(f => f.endsWith('.env'));
        // Sort by mtime desc to get the most recent session
        const withStats = files.map(f => {
          const fp = path.join(envDir, f);
          try { return { file: f, mtime: fs.statSync(fp).mtimeMs }; } catch { return null; }
        }).filter(Boolean) as { file: string; mtime: number }[];
        withStats.sort((a, b) => b.mtime - a.mtime);

        for (const { file } of withStats) {
          const fp = path.join(envDir, file);
          const content = fs.readFileSync(fp, 'utf-8');
          const match = content.match(/^IMMORTERM_ID=(.+)$/m);
          if (match && match[1] === ctx.sourceImmorTermId) {
            // Filename (minus .env) is the Claude Code / OpenMemory session UUID
            ctx.sourceSessionId = file.replace('.env', '');
            break;
          }
        }
      } catch {
        // Best effort — don't fail task creation over this
      }
    }

    return ctx;
  }

  /**
   * Fire-and-forget: fetch the latest session summary memory_id for the task's
   * origin session and patch `task.context.sourceMemorySummaryId` so the
   * task-drop hook (`immorterm-task-context.sh`) can emit a valid
   * `get_memory_context(memory_id=...)` call when the task is dragged onto
   * another session.
   *
   * Falls back to the most-recent session fact if no explicit summary version exists.
   */
  private async resolveTaskMemorySummaryId(taskId: string): Promise<void> {
    if (!this.taskStorage) return;
    const task = this.taskStorage.getById(taskId);
    if (!task) return;
    const sessionId = task.context?.sourceSessionId;
    const immortermId = task.context?.sourceImmorTermId;
    if (!sessionId && !immortermId) return;
    if (task.context?.sourceMemorySummaryId) return; // already resolved

    try {
      const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
      const userId = getStableProjectId(this.projectPath);
      const qs = sessionId
        ? `session_id=${encodeURIComponent(sessionId)}`
        : `immorterm_id=${encodeURIComponent(immortermId!)}`;
      const url = `${getMemoryUrl()}/api/v1/sessions/context?${qs}&user_id=${encodeURIComponent(userId)}`;
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 3000);
      const res = await fetch(url, { signal: controller.signal });
      clearTimeout(timeout);
      if (!res.ok) return;
      type MemoryRecord = {
        id: string;
        byte_offset?: number;
        byte_length?: number;
        jsonl_path?: string;
      };
      const data = await res.json() as {
        summary_versions?: MemoryRecord[];
        facts?: MemoryRecord[];
      };
      const source = data.summary_versions?.[0] ?? data.facts?.[0];
      if (!source) return;
      const memoryId = source.id;
      if (!memoryId) return;

      const byteOffset = typeof source.byte_offset === 'number' ? source.byte_offset : undefined;
      const byteLength = typeof source.byte_length === 'number' ? source.byte_length : undefined;
      const jsonlPath = typeof source.jsonl_path === 'string' ? source.jsonl_path : undefined;

      const current = this.taskStorage.getById(taskId);
      if (!current) return;
      const mergedContext: TaskContext = {
        ...(current.context || {}),
        sourceMemorySummaryId: memoryId,
        ...(byteOffset !== undefined && { sourceMemoryByteOffset: byteOffset }),
        ...(byteLength !== undefined && { sourceMemoryByteLength: byteLength }),
        ...(jsonlPath && { sourceMemoryJsonlPath: jsonlPath }),
      };
      this.taskStorage.update(taskId, { context: mergedContext });
    } catch {
      // Best effort — task is usable without this field
    }
  }

  /**
   * AI-enrich a newly created task using Haiku.
   * Gathers session context (summary, selected text, terminal surroundings)
   * and runs `claude -p --model haiku` in background to suggest description, type, lane.
   */
  private async aiEnrichTask(task: Task): Promise<void> {
    const activeSession = [...this.sessions.values()].find(s => s.windowId);
    const cwd = this.projectPath || process.cwd();
    const sessionName = activeSession?.displayName || 'unknown';
    const windowId = activeSession?.windowId;

    // Gather context pieces in parallel
    const contextParts: string[] = [];
    contextParts.push(`Project: ${cwd}`);
    contextParts.push(`Session: ${sessionName}`);

    // 1. Session summary from memory service (if available)
    if (windowId) {
      try {
        const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
        const userId = getStableProjectId(this.projectPath);
        const url = `${getMemoryUrl()}/api/v1/sessions/context?immorterm_id=${encodeURIComponent(windowId)}&user_id=${encodeURIComponent(userId)}`;
        const controller = new AbortController();
        const timeout = setTimeout(() => controller.abort(), 3000);
        const res = await fetch(url, { signal: controller.signal });
        clearTimeout(timeout);
        if (res.ok) {
          const data = await res.json() as { summary?: string; facts?: { content?: string }[] };
          if (data.summary) {
            contextParts.push(`\nSession summary:\n${data.summary}`);
          }
          if (data.facts && data.facts.length > 0) {
            contextParts.push(`\nKey facts:\n${data.facts.slice(0, 10).map(f => `- ${f.content || f}`).join('\n')}`);
          }
        }
      } catch {
        // Memory service unavailable — continue without summary
      }
    }

    // 2. Task context: selected text + surrounding terminal content
    if (task.context?.selectedText) {
      contextParts.push(`\nSelected text from terminal:\n\`\`\`\n${task.context.selectedText}\n\`\`\``);
    }

    const contextBlock = contextParts.join('\n');
    const prompt = `You are a task enrichment assistant for a developer's task board.
Given a task title and session context, generate:
1. A concise markdown description (2-4 lines) explaining what this task involves, informed by the session context
2. A suggested type: bug, feature, investigate, or other
3. A suggested lane: now (urgent/blocking), next (soon), later (backlog)

Task title: "${task.title}"

${contextBlock}

Return ONLY a JSON object with these fields:
{"description": "markdown description here", "type": "bug|feature|investigate|other", "lane": "now|next|later"}`;

    try {
      // Dispatches through immorterm-p (subscription-safe). The wrapper runs
      // interactive claude in a headless immorterm session and returns the
      // model's unwrapped answer on stdout. We DON'T pass --output-format json
      // because the wrapper has already stripped that layer.
      //
      // History: this was first migrated in f89ddaca (May 14), reverted in
      // session 98770331-9f6c-4297-b5be-eb786aea5cf2 (May 16) during a
      // "speed up haiku" effort that mistakenly stripped the wrapper layer.
      // Re-applied 2026-05-17 — the wrapper IS the way we keep working when
      // Anthropic pulls `claude -p` from the subscription tier. Speed concerns
      // are addressed elsewhere (cache reuse, `--strict-mcp-config`).
      const immortermP = process.env.HOME
        ? path.join(process.env.HOME, '.immorterm', 'bin', 'immorterm-p')
        : 'immorterm-p';
      const child = spawn(immortermP, [
        '--permission-mode', 'bypassPermissions',
        '--model', 'haiku',
        '--allowed-tools', 'Write',
        '--disable-slash-commands',
        prompt,
      ], { stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, IMMORTERM_P_TIMEOUT: '55' } });

      let stdout = '';
      let stderr = '';
      child.stdout?.on('data', (data: Buffer) => { stdout += data.toString(); });
      child.stderr?.on('data', (data: Buffer) => { stderr += data.toString(); });
      child.on('close', (code: number | null) => {
        if (code !== 0 || !stdout.trim()) {
          logger.warn(`ImmorTerm AI: task enrich failed (exit ${code}, stderr: ${stderr.slice(0, 200)})`);
          return;
        }
        try {
          // immorterm-p stdout IS the model's result string (already unwrapped).
          // The model may have wrapped its JSON in markdown fences — strip them.
          let resultStr = stdout.trim();
          resultStr = resultStr.replace(/^```(?:json)?\s*\n?/i, '').replace(/\n?```\s*$/i, '').trim();
          const inner = JSON.parse(resultStr);
          const updates: Record<string, unknown> = {};
          if (inner.description && typeof inner.description === 'string') {
            updates.description = inner.description;
          }
          if (['bug', 'feature', 'investigate', 'other'].includes(inner.type)) {
            updates.type = inner.type;
          }
          if (['now', 'next', 'later'].includes(inner.lane)) {
            updates.lane = inner.lane;
          }
          if (Object.keys(updates).length > 0 && this.taskStorage) {
            this.taskStorage.update(task.id, updates as any);
            this.sendTasksToWebview();
            // Trigger sparkle animation in webview
            this.view?.webview.postMessage({ type: 'task-ai-enhanced', taskId: task.id });
            logger.info(`ImmorTerm AI: task '${task.title}' enriched by AI (${Object.keys(updates).join(', ')})`);
          }
        } catch (parseErr) {
          logger.debug(`ImmorTerm AI: task enrich parse error: ${parseErr}`);
        }
      });

      // 60s timeout: immorterm-p needs ~15-20s for REPL boot + dialog dismiss
      // on top of model latency. Generous bound to avoid zombie procs.
      setTimeout(() => { try { child.kill(); } catch {} }, 60_000);
    } catch (err) {
      logger.debug(`ImmorTerm AI: failed to spawn immorterm-p for task enrich: ${err}`);
    }
  }

  /**
   * Haiku fallback for the explore-selection popup. Dispatches through
   * `immorterm-p`, which runs an interactive `claude` inside a headless
   * immorterm session (subscription-tier safe, vendor-abstracted). The
   * wrapper writes the model's answer to a temp JSON file via the Write
   * tool and prints the unwrapped result on stdout. Posts the answer as a
   * partial then a "done" event, matching the free-source streaming path.
   *
   * History: first migrated in f89ddaca (May 14), reverted by session
   * 98770331-9f6c-4297-b5be-eb786aea5cf2 (May 16) during a "speed up haiku"
   * attempt that stripped the wrapper layer in pursuit of latency. Re-
   * applied 2026-05-17. Latency concerns belong to wrapper internals
   * (prompt cache reuse, --strict-mcp-config), NOT to bypassing the wrapper.
   */
  private explainSelectionWithHaiku(rawText: string, reqId: number, forceLLM = false): void {
    // Strip ANSI escape codes (color sequences from the terminal) and other
    // control chars upfront — sources don't tolerate \x1b[...m noise, and
    // the LLM doesn't need the visual formatting either.
    const cleanText = (rawText ?? '')
      .toString()
      .replace(/\x1b\[[0-9;]*[a-zA-Z]/g, '')
      .replace(/[\x00-\x08\x0b-\x1f\x7f]/g, '')
      .trim();
    type Hit = { source: string; text: string; image?: string };
    // Streaming protocol: each source posts a {partial:true, result} message
    // as it resolves so the popup can show drawers immediately. A final
    // {partial:false, anyHit} signals the end. The popup uses partial:false
    // with anyHit=false (and no prior partials) to render the error state.
    const post = (data: Record<string, unknown>) => {
      this.view?.webview.postMessage({ type: 'explain-result', reqId, ...data });
    };
    const sendPartial = (r: Hit) => post({ partial: true, ok: true, result: r });
    const sendDone = (anyHit: boolean, error?: string) =>
      post({ partial: false, ok: anyHit, anyHit, ...(error ? { error } : {}) });

    if (!cleanText) {
      sendDone(false, 'Empty selection.');
      return;
    }
    logger.debug(`ImmorTerm AI: explain (${cleanText.length} chars, forceLLM=${forceLLM}): ${cleanText.slice(0, 80)}`);

    if (forceLLM) {
      this.runClaudeExplainCLI(cleanText, sendPartial, sendDone);
      return;
    }

    if (cleanText.length <= 500) {
      let anyHit = false;
      const tasks: Promise<Hit | null>[] = [
        this.fetchDictionary(cleanText),
        this.fetchWikipedia(cleanText),
        this.fetchDDG(cleanText),
        this.fetchUrbanDictionary(cleanText),
      ];
      // Fire-and-forward each source. The .then below resolves to undefined
      // because we already side-effected via sendPartial; Promise.allSettled
      // just waits for everything to settle so we know when to send "done".
      tasks.forEach((p, idx) => {
        p.then((r) => {
          if (r) {
            anyHit = true;
            sendPartial(r);
            logger.debug(`ImmorTerm AI: explain partial[${idx}] from ${r.source}`);
          }
        }).catch(() => { /* fetchers already swallow errors */ });
      });
      Promise.allSettled(tasks).then(() => {
        if (anyHit) {
          sendDone(true);
        } else {
          this.runClaudeExplainCLI(cleanText, sendPartial, sendDone);
        }
      });
      return;
    }

    this.runClaudeExplainCLI(cleanText, sendPartial, sendDone);
  }

  /** Free Dictionary API (api.dictionaryapi.dev). No auth, Wiktionary-backed.
   *  Only useful for single English words — skip multi-word queries to avoid
   *  noisy 404s. Returns null on miss/error. */
  private fetchDictionary(query: string): Promise<{ text: string; source: string } | null> {
    if (!/^[A-Za-z][A-Za-z'-]{1,40}$/.test(query)) {
      logger.debug('ImmorTerm AI: [dict] skip (not single-word)');
      return Promise.resolve(null);
    }
    return this.httpsGetJson(`https://api.dictionaryapi.dev/api/v2/entries/en/${encodeURIComponent(query.toLowerCase())}`)
      .then((data) => {
        if (!Array.isArray(data) || !data[0]) { logger.debug('ImmorTerm AI: [dict] empty'); return null; }
        const meanings = Array.isArray(data[0].meanings) ? data[0].meanings : [];
        const lines: string[] = [];
        for (const m of meanings.slice(0, 2)) {
          const pos = String(m.partOfSpeech || '').trim();
          const def = m.definitions && m.definitions[0] && m.definitions[0].definition;
          if (def) lines.push((pos ? `(${pos}) ` : '') + String(def).trim());
        }
        const text = lines.join('\n').trim();
        if (!text) { logger.debug('ImmorTerm AI: [dict] no usable text'); return null; }
        logger.debug(`ImmorTerm AI: [dict] hit (${text.length} chars)`);
        return { text, source: 'Free Dictionary · Wiktionary' };
      })
      .catch((err) => { logger.debug(`ImmorTerm AI: [dict] err: ${err && err.message || err}`); return null; });
  }

  /** Wikipedia REST API page-summary. Returns the lead-paragraph extract.
   *  Best for proper nouns, technical terms, multi-word concepts. */
  private fetchWikipedia(query: string): Promise<{ text: string; source: string; image?: string } | null> {
    const title = query.length > 0 ? query[0].toUpperCase() + query.slice(1) : query;
    return this.httpsGetJson(`https://en.wikipedia.org/api/rest_v1/page/summary/${encodeURIComponent(title)}?redirect=true`)
      .then((data) => {
        if (!data || typeof data !== 'object') { logger.debug('ImmorTerm AI: [wiki] empty'); return null; }
        if (data.type === 'disambiguation') { logger.debug('ImmorTerm AI: [wiki] disambiguation'); return null; }
        const extract = String(data.extract || '').trim();
        if (extract.length < 20) { logger.debug(`ImmorTerm AI: [wiki] short extract (${extract.length})`); return null; }
        const concise = extract.split(/(?<=[.!?])\s+/).slice(0, 3).join(' ');
        // The summary payload already carries a lead image — surface it (https only).
        const img = String(data.thumbnail?.source || data.originalimage?.source || '').trim();
        const image = img.startsWith('https:') ? img : undefined;
        logger.debug(`ImmorTerm AI: [wiki] hit (${concise.length} chars${image ? ', +image' : ''})`);
        return { text: concise, source: 'Wikipedia', image };
      })
      .catch((err) => { logger.debug(`ImmorTerm AI: [wiki] err: ${err && err.message || err}`); return null; });
  }

  /** DuckDuckGo Instant Answer (existing path). Sometimes catches things
   *  Wikipedia misses (e.g. acronyms with structured DDG data). */
  private fetchDDG(query: string): Promise<{ text: string; source: string; image?: string } | null> {
    return this.httpsGetJson(`https://api.duckduckgo.com/?q=${encodeURIComponent(query)}&format=json&no_html=1&skip_disambig=1&t=immorterm`)
      .then((data) => {
        if (!data) { logger.debug('ImmorTerm AI: [ddg] empty'); return null; }
        if (data.Type === 'D') { logger.debug('ImmorTerm AI: [ddg] disambiguation'); return null; }
        const summary = String(data.AbstractText || data.Abstract || data.Definition || '').trim();
        if (summary.length < 20) { logger.debug(`ImmorTerm AI: [ddg] short summary (${summary.length})`); return null; }
        const src = String(data.AbstractSource || data.DefinitionSource || 'DuckDuckGo').trim();
        const concise = summary.split(/(?<=[.!?])\s+/).slice(0, 3).join(' ');
        // DDG's Image is often a site-relative path (/i/...); make it absolute.
        const rawImg = String(data.Image || '').trim();
        const image = rawImg.startsWith('https:') ? rawImg
          : rawImg.startsWith('/') ? `https://duckduckgo.com${rawImg}`
          : undefined;
        logger.debug(`ImmorTerm AI: [ddg] hit (${concise.length} chars${image ? ', +image' : ''})`);
        return { text: concise, source: `DuckDuckGo · ${src}`, image };
      })
      .catch((err) => { logger.debug(`ImmorTerm AI: [ddg] err: ${err && err.message || err}`); return null; });
  }

  /** Urban Dictionary for slang. The public v0 API stopped exposing reliable
   *  vote counts (everything returns thumbs_up=thumbs_down=0 as of 2026),
   *  so we trust UD's own server-side ordering — the first entry is their
   *  canonical pick. Length-gated to avoid both empty stubs and walls of
   *  text; the [bracket] decorations UD uses around hyperlinks are stripped. */
  private fetchUrbanDictionary(query: string): Promise<{ text: string; source: string } | null> {
    if (query.length > 40) { logger.debug('ImmorTerm AI: [urban] skip (>40 chars)'); return Promise.resolve(null); }
    // UD is the slowest source (cold call ~1s). Give it a longer budget so
    // it doesn't get hammered by the global timeout under network jitter.
    return this.httpsGetJson(`https://api.urbandictionary.com/v0/define?term=${encodeURIComponent(query)}`, 4000)
      .then((data) => {
        const list: Array<{ definition?: string }> = data && Array.isArray(data.list) ? data.list : [];
        if (!list.length || !list[0]) { logger.debug('ImmorTerm AI: [urban] empty list'); return null; }
        const def = String(list[0].definition || '').replace(/\[|\]/g, '').replace(/\r\n?/g, '\n').trim();
        if (def.length < 10) { logger.debug(`ImmorTerm AI: [urban] too short (${def.length})`); return null; }
        // Truncate (don't reject) overly long definitions so a wall-of-text
        // entry still surfaces, just trimmed to two sentences.
        const concise = def.split(/(?<=[.!?])\s+/).slice(0, 2).join(' ').slice(0, 600);
        logger.debug(`ImmorTerm AI: [urban] hit (${concise.length} chars)`);
        return { text: concise, source: 'Urban Dictionary' };
      })
      .catch((err) => { logger.debug(`ImmorTerm AI: [urban] err: ${err && err.message || err}`); return null; });
  }

  /** Shared JSON GET. Resolves to parsed object, rejects on http >=400,
   *  timeout, network error, or invalid JSON. Default 3s — Urban Dictionary
   *  can take 1s+ on a cold connection, so 1.5s was too tight in practice.  */
  private httpsGetJson(url: string, timeoutMs = 3000): Promise<any> {
    return new Promise((resolve, reject) => {
      const req = https.get(url, {
        timeout: timeoutMs,
        headers: { 'User-Agent': 'ImmorTerm/1.0 (explore-selection)', 'Accept': 'application/json' },
      }, (res) => {
        const status = res.statusCode ?? 0;
        if (status >= 400) { res.resume(); return reject(new Error(`http ${status}`)); }
        const chunks: Buffer[] = [];
        res.on('data', (c: Buffer) => chunks.push(c));
        res.on('end', () => {
          try { resolve(JSON.parse(Buffer.concat(chunks).toString('utf8'))); }
          catch (e) { reject(e); }
        });
      });
      req.on('timeout', () => { req.destroy(new Error('timeout')); });
      req.on('error', reject);
    });
  }

  /** The slim `claude -p` Haiku fallback. Posts a single partial with the
   *  answer and then a "done" event so the popup follows the same protocol
   *  the free-source streaming path uses. */
  private runClaudeExplainCLI(
    text: string,
    sendPartial: (r: { source: string; text: string }) => void,
    sendDone: (anyHit: boolean, error?: string) => void,
  ): void {
    const MAX_INPUT_CHARS = 2000;
    const truncated = text.length > MAX_INPUT_CHARS
      ? text.slice(0, MAX_INPUT_CHARS) + '\n[…truncated]'
      : text;

    const prompt = `What does this mean? Reply in 1 short sentence, plain text, no preamble.\n\n${truncated}`;

    try {
      const immortermP = process.env.HOME
        ? path.join(process.env.HOME, '.immorterm', 'bin', 'immorterm-p')
        : 'immorterm-p';
      // Flags forwarded verbatim to interactive `claude` inside the headless
      // session. `Write` must be in --allowed-tools so the wrapper can harvest
      // the response from its temp file. `--permission-mode bypassPermissions`
      // is required for the wrapper's dialog auto-dismissal to apply.
      const args = [
        '--permission-mode', 'bypassPermissions',
        '--model', 'haiku',
        '--allowed-tools', 'Write',
        '--disable-slash-commands',
        '--append-system-prompt', 'Answer in one short sentence. Plain text only. No preamble. No markdown.',
        prompt,
      ];

      const child = spawn(immortermP, args, {
        stdio: ['pipe', 'pipe', 'pipe'],
        env: { ...process.env, IMMORTERM_P_TIMEOUT: '45' },
        cwd: os.tmpdir(),
      });

      let stdout = '';
      let stderr = '';
      let done = false;
      child.stdout?.on('data', (data: Buffer) => { stdout += data.toString(); });
      child.stderr?.on('data', (data: Buffer) => { stderr += data.toString(); });
      child.on('close', (code: number | null) => {
        if (done) return;
        done = true;
        if (code !== 0 || !stdout.trim()) {
          logger.warn(`ImmorTerm AI: explain-selection failed (exit ${code}, stderr: ${stderr.slice(0, 200)})`);
          sendDone(false, `Haiku exited ${code}.`);
          return;
        }
        const answer = stdout.trim().replace(/^```(?:[\w-]+)?\s*\n?/i, '').replace(/\n?```\s*$/i, '').trim();
        if (!answer) {
          sendDone(false, 'Haiku returned an empty answer.');
          return;
        }
        sendPartial({ source: 'Claude Haiku', text: answer });
        sendDone(true);
      });

      setTimeout(() => {
        if (done) return;
        try { child.kill(); } catch {}
        done = true;
        sendDone(false, 'Haiku request timed out (50s).');
      }, 50_000);
    } catch (err) {
      logger.debug(`ImmorTerm AI: failed to spawn immorterm-p for explain-selection: ${err}`);
      sendDone(false, `Could not spawn immorterm-p: ${err instanceof Error ? err.message : String(err)}`);
    }
  }

  /** Restore sessions from the daemon registry.
   *  Only restores AI sessions belonging to THIS project.
   *  Uses the same windowId/displayName architecture as the regular terminal.
   *
   *  If a daemon is dead (reboot, crash), spawns a fresh daemon with the same
   *  session name so the log file is resumed and scrollback is preserved.
   *  This mirrors how regular terminals work via screen-auto.
   */
  async restoreSessions(): Promise<void> {
    const p = this.doRestoreSessions();
    this.restorePromise = p;
    return p;
  }

  private async doRestoreSessions(): Promise<void> {
    try {
      const registryPath = path.join(process.env.HOME || '~', '.immorterm', 'registry.json');
      if (!fs.existsSync(registryPath)) return;

      const registry = JSON.parse(fs.readFileSync(registryPath, 'utf-8'));
      if (!registry.sessions || !Array.isArray(registry.sessions)) return;

      // Resolve our own project identity. ownerProjectId is the primary
      // key; falls back to projectPath comparison if the project-id file
      // doesn't exist yet (brand-new project, first session never spawned).
      this.ownerProjectId = readProjectId(this.projectPath);

      // Pre-filter cross-project enrichment from each entry's own session.json.
      // session.json is daemon-EXCLUSIVE (single writer, no race), so the new
      // owner_project_dir / owner_project_id / worktree fields survive there
      // even while old-binary daemons running in OTHER windows strip them from
      // registry.json on their 10s registry rewrites. We patch the in-memory
      // entries here so the filter below sees the truth.
      let sjPatched = 0;
      for (const session of registry.sessions) {
        if (!session.structured_log_dir) continue;
        if (session.owner_project_dir && session.owner_project_id) continue;
        const sjPath = path.join(session.structured_log_dir, 'session.json');
        try {
          if (!fs.existsSync(sjPath)) continue;
          const sj = JSON.parse(fs.readFileSync(sjPath, 'utf-8'));
          let patched = false;
          if (!session.owner_project_dir && sj.owner_project_dir) {
            session.owner_project_dir = sj.owner_project_dir;
            patched = true;
          }
          if (!session.owner_project_id && sj.owner_project_id) {
            session.owner_project_id = sj.owner_project_id;
            patched = true;
          }
          if (!session.worktree && sj.worktree) {
            session.worktree = sj.worktree;
            patched = true;
          }
          if (patched) sjPatched++;
        } catch { /* malformed session.json — skip */ }
      }
      if (sjPatched > 0) {
        logger.info(`ImmorTerm AI: enriched ${sjPatched} registry entries from session.json (owner_project / worktree)`);
      }

      // Inline derive owner_project_dir for entries that STILL lack it after
      // session.json enrichment. This is necessary because backfill writes
      // to registry.json on disk — but old-binary daemons strip those fields
      // back out within ~50ms of the write. Resolving in-memory here makes
      // the filter race-proof: each entry's owner is derived from its
      // project_dir via git common-dir, cached per unique project_dir so
      // we shell out at most ~10 times across 200+ entries.
      const ownerCache = new Map<string, { ownerDir: string; worktree: string | undefined }>();
      let derivedCount = 0;
      for (const session of registry.sessions) {
        if (session.owner_project_dir) continue;
        if (!session.project_dir) continue;
        let resolved = ownerCache.get(session.project_dir);
        if (!resolved) {
          resolved = resolveOwnerProjectFromPath(session.project_dir);
          ownerCache.set(session.project_dir, resolved);
        }
        if (resolved.ownerDir) {
          session.owner_project_dir = resolved.ownerDir;
          if (!session.worktree && resolved.worktree) {
            session.worktree = resolved.worktree;
          }
          // Also derive owner_project_id from the resolved owner_dir's
          // project-id file (the daemon or backfill already created it).
          if (!session.owner_project_id) {
            const pid = readProjectId(resolved.ownerDir);
            if (pid) session.owner_project_id = pid;
          }
          derivedCount++;
        }
      }
      if (derivedCount > 0) {
        logger.info(`ImmorTerm AI: derived owner_project_dir for ${derivedCount} entries via git resolution (race defense)`);
      }

      const prefix = `${this.projectName}-ai-`;
      logger.debug(`ImmorTerm AI restore: ${registry.sessions.length} registry entries, filtering for project '${this.projectName}' (id: ${this.ownerProjectId || 'none'})`);

      // Read session-status.json directly — initRegistryClient may not have run yet
      // (restoreSessions() is called before initRegistryClient() in extension.ts)
      let shelvedSet: Set<string> = new Set();
      try {
        const statusPath = path.join(process.env.HOME || '~', '.immorterm', 'session-status.json');
        if (fs.existsSync(statusPath)) {
          const statusData = JSON.parse(fs.readFileSync(statusPath, 'utf-8'));
          for (const [wid, entry] of Object.entries(statusData.sessions || {})) {
            if ((entry as any).status === 'shelved') shelvedSet.add(wid);
          }
        }
      } catch { /* best effort */ }

      // First pass: collect matching AI sessions.
      // Also count dead-pid'd entries to detect cold boot — used below to
      // decide whether to run the orphan-dir reconciler (skip on cold boot).
      let deadAtStart = 0;
      let aiSeen = 0;
      const aiSessions: typeof registry.sessions = [];
      for (const session of registry.sessions) {
        if (!session.name) {
          logger.debug(`ImmorTerm AI restore: skipping entry with no name (pid: ${session.pid})`);
          continue;
        }
        const isAi = session.session_type === 'ai' || session.name.startsWith(prefix);
        if (!isAi) {
          logger.debug(`ImmorTerm AI restore: skipping '${session.name}' — not AI type (session_type: ${session.session_type || 'none'})`);
          continue;
        }
        if (shelvedSet.has(session.window_id)) {
          logger.debug(`ImmorTerm AI restore: skipping '${session.name}' — shelved`);
          continue;
        }
        // Project match — three tiers, most-stable first:
        //   1. owner_project_id — UUID from .immorterm/project-id, rename- and machine-portable
        //   2. owner_project_dir — explicit immutable trunk path written at spawn
        //   3. project_dir       — legacy fallback for entries written before this migration
        // The name-prefix fallback (session.name.startsWith(prefix)) is intentionally
        // dropped — it falsely matched worktree-spawned daemons whose name prefix is
        // the worktree's basename (e.g. "speak-mode-ai-*"), and falsely missed daemons
        // whose owner project shares no name with the worktree basename.
        let matchesProject = false;
        if (this.ownerProjectId && session.owner_project_id) {
          matchesProject = session.owner_project_id === this.ownerProjectId;
        } else if (session.owner_project_dir) {
          matchesProject = session.owner_project_dir === this.projectPath;
        } else {
          matchesProject = session.project_dir === this.projectPath;
        }
        if (!matchesProject) {
          logger.debug(`ImmorTerm AI restore: skipping '${session.name}' — wrong project (owner_id: ${session.owner_project_id || 'none'}, owner_dir: ${session.owner_project_dir || 'none'}, project_dir: ${session.project_dir || 'none'})`);
          continue;
        }
        aiSessions.push(session);
        aiSeen++;
        if (!session.pid || !isProcessAlive(session.pid)) deadAtStart++;
      }

      // Cold-boot detection: if >50% of AI sessions had a dead pid at the
      // start of restore, every spawn is racing through a ws_port wait at
      // the same time, and some will miss the 8s window. Sessions that
      // miss return null from the restore job → would normally be archived
      // by reconcileOrphanedAiDirs at end of restore → daemon self-heals
      // back into registry on next tick → but the dir was archived and
      // the registry entry's old log path is dangling. Skipping the
      // reconciler on cold boot lets the slow daemons recover cleanly.
      const isColdBoot = aiSeen > 0 && deadAtStart * 2 > aiSeen;
      if (isColdBoot) {
        logger.info(`ImmorTerm AI: cold boot detected (${deadAtStart}/${aiSeen} dead pids) — orphan reconciler will be skipped`);
      }

      // Enrich from session.json — recover display names lost to registry write races.
      // session.json is daemon-exclusive (no write contention) and lives in the structured log dir.
      try {
        const logDir = path.join(this.projectPath, '.immorterm', 'terminals', 'logs');
        if (fs.existsSync(logDir)) {
          const logEntries = fs.readdirSync(logDir);
          // Build map: window_id → most recent session.json data
          const sjMap = new Map<string, any>();
          for (const dirName of logEntries) {
            const sjPath = path.join(logDir, dirName, 'session.json');
            if (!fs.existsSync(sjPath)) continue;
            try {
              const sj = JSON.parse(fs.readFileSync(sjPath, 'utf-8'));
              if (sj.session_type !== 'ai' || !sj.window_id) continue;
              // Keep latest (dir names are date-prefixed, lexicographic sort works)
              const prev = sjMap.get(sj.window_id);
              if (!prev || dirName > prev._dir) {
                sj._dir = dirName;
                sjMap.set(sj.window_id, sj);
              }
            } catch { /* malformed session.json — skip */ }
          }

          // Enrich existing registry entries and lock custom names so Claude doesn't overwrite them.
          // Also recover claude_session_id — registry.json races between daemon + extension writers
          // and frequently loses this field, breaking auto-resume on reboot. session.json is
          // daemon-exclusive (single writer) so it's the authoritative source.
          for (const session of aiSessions) {
            const sj = sjMap.get(session.window_id);
            if (!sj) continue;
            if ((!session.display_name || session.display_name === 'zsh') && sj.display_name) {
              logger.info(`ImmorTerm AI: recovering display name '${sj.display_name}' for '${session.name}' from session.json`);
              session.display_name = sj.display_name;
              if (sj.display_name !== 'zsh' && !/^immorterm-\d+$/.test(sj.display_name)) {
                session.title_locked = true;
                updateRegistryTitleLocked(session.window_id, true);
              }
            }
            if (!session.claude_session_id && sj.claude_session_id) {
              logger.info(`ImmorTerm AI: recovering claude_session_id from session.json for '${session.name}' (race-wiped from registry)`);
              session.claude_session_id = sj.claude_session_id;
              try { updateClaudeSessionId(session.window_id, sj.claude_session_id); } catch { /* best effort */ }
            }
          }

          // NOTE: We intentionally do NOT discover sessions missing from registry here.
          // With hundreds of historical session.json files, any time-based heuristic is too noisy.
          // Missing registry entries will be addressed by daemon self-registration on respawn.
        }
      } catch (e) {
        logger.warn(`ImmorTerm AI: session.json enrichment failed: ${e}`);
      }

      // Tier 3 — claude-env mtime scan for sessions STILL missing claude_session_id after
      // registry + session.json lookups. SessionStart hook writes ~/.immorterm/claude-env/<uuid>.env
      // on every Claude start, where the filename IS the Claude UUID and the contents carry
      // IMMORTERM_ID=<window_id>. Newest mtime for a given window_id = the most recent Claude
      // session in that terminal. This is the ground-truth fallback used before auto-resume.
      try {
        const stillMissing = aiSessions.filter((s: any) => !s.claude_session_id && s.window_id);
        if (stillMissing.length > 0) {
          const envDir = path.join(process.env.HOME || '~', '.immorterm', 'claude-env');
          if (fs.existsSync(envDir)) {
            const byWid = new Map<string, { uuid: string; mtimeMs: number }>();
            const entries = fs.readdirSync(envDir);
            for (const name of entries) {
              if (!name.endsWith('.env')) continue;
              const full = path.join(envDir, name);
              try {
                const contents = fs.readFileSync(full, 'utf-8');
                const match = contents.match(/^IMMORTERM_ID=(\S+)/m);
                if (!match) continue;
                const wid = match[1];
                const stat = fs.statSync(full);
                const prev = byWid.get(wid);
                if (!prev || stat.mtimeMs > prev.mtimeMs) {
                  byWid.set(wid, { uuid: name.replace(/\.env$/, ''), mtimeMs: stat.mtimeMs });
                }
              } catch { /* skip unreadable */ }
            }
            for (const session of stillMissing) {
              const hit = byWid.get(session.window_id);
              if (hit) {
                logger.info(`ImmorTerm AI: recovering claude_session_id ${hit.uuid} from claude-env for '${session.name}' (newest mtime)`);
                session.claude_session_id = hit.uuid;
                try { updateClaudeSessionId(session.window_id, hit.uuid); } catch { /* best effort */ }
              }
            }
          }
        }
      } catch (e) {
        logger.warn(`ImmorTerm AI: claude-env resolver failed: ${e}`);
      }

      // Sort by session_order from session-status.json (daemon-safe, not registry.json).
      // Read directly from file because initRegistryClient may not have run yet.
      let orderMap: Record<string, number> = {};
      try {
        const statusPath = path.join(process.env.HOME || '~', '.immorterm', 'session-status.json');
        if (fs.existsSync(statusPath)) {
          const statusData = JSON.parse(fs.readFileSync(statusPath, 'utf-8'));
          for (const [wid, entry] of Object.entries(statusData.sessions || {})) {
            if ((entry as any).session_order != null) orderMap[wid] = (entry as any).session_order;
          }
        }
      } catch (e) { logger.warn(`ImmorTerm AI: failed to read session order: ${e}`); }
      aiSessions.sort((a: any, b: any) => (orderMap[a.window_id] ?? Infinity) - (orderMap[b.window_id] ?? Infinity));

      // Resolve daemon binary once — shared across all parallel restores
      const binary = await findDaemonBinary();

      // Scaled ws_port timeout — concurrent respawns starve each other for
      // CPU/IO during cold boot, so a single daemon's spawn-to-first-write
      // latency grows roughly linearly with the number of peers also spawning.
      // 8s is fine for one daemon. With ~20 peers it routinely misses.
      // Formula: 8000ms base + 1000ms per peer, capped at 30s.
      const respawnCount = aiSessions.filter((s: any) => !s.pid || !isProcessAlive(s.pid)).length;
      const wsPortTimeoutMs = Math.min(30000, 8000 + 1000 * Math.max(0, respawnCount - 1));
      if (respawnCount > 1) {
        logger.info(`ImmorTerm AI: ${respawnCount} daemons need respawn — ws_port timeout scaled to ${wsPortTimeoutMs}ms each`);
      }

      // Concurrency cap for respawns. Live-reconnect jobs (warm path) are
      // cheap and run unbounded; respawns hit cargo-style CPU/IO contention.
      // After the owner_project_dir backfill collapses worktree-rooted
      // entries into the trunk project, a single project may legitimately
      // have 100+ entries — most stale dead-pid leftovers. Blasting all of
      // them at the kernel at once causes spawn timeouts and zombies. Cap
      // active respawns at RESPAWN_CONCURRENCY; warm reconnects are
      // independent (they sit in this.sessions immediately).
      const RESPAWN_CONCURRENCY = 5;
      let activeRespawns = 0;
      const respawnQueue: Array<() => void> = [];
      const acquireRespawnSlot = (): Promise<void> =>
        new Promise((resolve_) => {
          const tryAcquire = () => {
            if (activeRespawns < RESPAWN_CONCURRENCY) {
              activeRespawns++;
              resolve_();
            } else {
              respawnQueue.push(tryAcquire);
            }
          };
          tryAcquire();
        });
      const releaseRespawnSlot = () => {
        activeRespawns--;
        const next = respawnQueue.shift();
        if (next) next();
      };

      // Restore sessions in parallel — daemon spawns and WS port waits run concurrently
      const restoreJobs = aiSessions.map((session: any, idx: number) => {
        const restoreCount = idx + 1;
        const displayName = (session.display_name && session.display_name !== session.name)
          ? session.display_name
          : `immorterm-${restoreCount}`;
        const windowId = session.window_id || '';
        const titleLocked = session.title_locked || false;
        const needsAttention = session.needs_attention || false;
        const claudeSessionId = session.claude_session_id || '';

        return (async (): Promise<{ name: string; wsPort: number; displayName: string; windowId: string; titleLocked: boolean; needsAttention: boolean; daemonPid: number | undefined; projectDir: string } | null> => {
          let wsPort: number | null = null;
          let daemonPid: number | undefined = session.pid || undefined;

          if (session.pid && isProcessAlive(session.pid)) {
            // Daemon is alive — just reconnect
            wsPort = session.ws_port || findWsPort(session.name);
            if (!wsPort) {
              // ws_port missing — try respawning a fresh daemon instead of skipping
              logger.warn(`ImmorTerm AI: live daemon '${session.name}' has no ws_port — attempting respawn`);
              if (binary) {
                cleanupStaleWsFiles(session.name);
                // Preserve the session's ORIGINAL spawn dir — passing
                // this.projectPath would reattribute a worktree-spawned daemon
                // to the trunk on respawn, destroying its workspace identity.
                const respawnProjDir = session.project_dir || this.projectPath;
                const respawnPid = await spawnDaemon(binary, session.name, respawnProjDir, windowId, displayName, claudeSessionId, titleLocked);
                wsPort = await waitForWsPort(session.name, wsPortTimeoutMs, respawnPid);
                daemonPid = findDaemonPidFromWsFile(session.name);
              }
            }
            if (!wsPort) {
              logger.warn(`ImmorTerm AI: '${session.name}' — all ws_port recovery failed, skipping`);
              return null;
            }
            logger.info(`ImmorTerm AI: reconnecting to live '${displayName}' (pid: ${session.pid}, ws: ${wsPort})`);
          } else {
            // Daemon is dead — resurrect it with a fresh daemon process.
            // Throttle through the concurrency cap so 100+ stale entries
            // don't blast the kernel at once after the worktree backfill
            // surfaces them all into one project's restore set.
            await acquireRespawnSlot();
            try {
              logger.info(`ImmorTerm AI: daemon dead for '${displayName}' (pid: ${session.pid}), respawning...`);
              if (!binary) {
                logger.warn('ImmorTerm AI: cannot resurrect — daemon binary not found');
                return null;
              }

              // Clean up stale .ws files from the dead daemon before spawning.
              cleanupStaleWsFiles(session.name);

              // Preserve the session's ORIGINAL spawn dir (see warm-branch comment).
              const respawnProjDir2 = session.project_dir || this.projectPath;
              const respawnPid2 = await spawnDaemon(binary, session.name, respawnProjDir2, windowId, displayName, claudeSessionId, titleLocked);
              // Timeout scales with how many peers are also respawning — see
              // wsPortTimeoutMs computation above. With concurrency capped at
              // RESPAWN_CONCURRENCY the actual peer pressure is bounded.
              wsPort = await waitForWsPort(session.name, wsPortTimeoutMs, respawnPid2);
              if (!wsPort) {
                logger.warn(`ImmorTerm AI: respawned daemon for '${session.name}' did not start (no ws_port)`);
                return null;
              }
              daemonPid = findDaemonPidFromWsFile(session.name);
              logger.info(`ImmorTerm AI: resurrected '${displayName}' (daemon pid: ${daemonPid}, ws: ${wsPort})`);
            } finally {
              releaseRespawnSlot();
            }
          }

          return { name: session.name, wsPort, displayName, windowId, titleLocked, needsAttention, daemonPid, projectDir: session.project_dir as string };
        })();
      });

      // Wait for all sessions to restore concurrently, then add to map in order
      const results = await Promise.allSettled(restoreJobs);
      for (const result of results) {
        if (result.status === 'fulfilled' && result.value) {
          const { name, wsPort, displayName, windowId, titleLocked, needsAttention, daemonPid, projectDir } = result.value;
          const branch = projectDir ? detectGitBranch(projectDir) : undefined;
          this.sessions.set(name, { wsPort, displayName, windowId, titleLocked, needsAttention, daemonPid, projectDir, branch });
          if (projectDir) this.armSessionBranchWatcher(projectDir);
        }
      }

      logger.info(`ImmorTerm AI: restored ${this.sessions.size} sessions for project '${this.projectName}'`);

      // Startup reconciler: enforce the invariant that .immorterm/terminals/logs/
      // only holds dirs for CURRENTLY ACTIVE sessions. Every other session dir
      // belongs in archive/. Historically archiving only ran on interactive × click,
      // so orphan dirs piled up (10-100× more than sessions). This sweeps them
      // into archive/ at every VS Code reload. See task #25 / agent #21 diagnosis.
      //
      // Protected set = (a) successfully restored sessions in this.sessions, PLUS
      // (b) any registry entry for this project whose daemon pid is still alive
      // (covers sessions where restore failed but daemon survives). This guards
      // against archiving a dir whose grid.jsonl is still being actively written.
      if (this.projectPath && !isColdBoot) {
        try {
          const { reconcileOrphanedAiDirs } = await import('./commands/cleanup');
          const logsDir = path.join(this.projectPath, '.immorterm', 'terminals', 'logs');
          const liveWindowIds = new Set<string>();
          for (const entry of this.sessions.values()) {
            if (entry.windowId) liveWindowIds.add(entry.windowId);
          }
          // Also protect registry-alive daemons for this project.
          for (const session of registry.sessions) {
            if (session.session_type !== 'ai') continue;
            if (session.project_dir !== this.projectPath) continue;
            if (!session.window_id) continue;
            if (session.pid && isProcessAlive(session.pid)) {
              liveWindowIds.add(session.window_id);
            }
          }
          const archived = await reconcileOrphanedAiDirs(logsDir, liveWindowIds);
          if (archived > 0) {
            logger.info(`ImmorTerm AI: reconciled ${archived} orphan session dirs into archive/`);
          }
        } catch (err) {
          logger.warn(`ImmorTerm AI: reconciler failed: ${err}`);
        }
      } else if (isColdBoot) {
        logger.info('ImmorTerm AI: skipped orphan reconciler — cold boot detected, daemons may still be respawning');
      }
    } catch (err) {
      logger.warn(`ImmorTerm AI: session restore failed: ${err}`);
    }
  }

  get sessionCount(): number {
    return this.sessions.size;
  }

  /**
   * Shelve an AI session — kill daemon but preserve registry entry for reattach.
   *
   * Shelving keeps the registry entry and archives logs so the session can be
   * reattached later via the "Reattach Terminal" command.
   *
   * Daemons can't "detach" like screen sessions, so we kill the daemon and
   * respawn a fresh one on reattach. Claude conversation resumes via claude_session_id.
   */
  private async shelveSession(sessionName: string, forceHard: boolean = false): Promise<void> {
    const session = this.sessions.get(sessionName);
    if (!session) return;

    // Soft-shelve fast path: keep daemon alive, drop the tab from the UI,
    // schedule a TTL timer that calls shelveSession again with forceHard=true.
    // Reattach within TTL skips SIGTERM/archive entirely — instant WS
    // reconnect. Disabled by default; opt in via `immorterm.softShelveEnabled`.
    const cfg = vscode.workspace.getConfiguration('immorterm');
    const softEnabled = cfg.get<boolean>('softShelveEnabled', false);
    const daemonAlive = !!session.daemonPid && isProcessAlive(session.daemonPid);
    if (!forceHard && softEnabled && daemonAlive && session.windowId) {
      const ttlMin = Math.max(1, Math.min(1440,
        cfg.get<number>('softShelveTtlMinutes', 120),
      ));
      const ttlMs = ttlMin * 60 * 1000;

      logger.info(`ImmorTerm AI: SOFT shelving '${sessionName}' (pid: ${session.daemonPid}, ttl: ${ttlMin}min)`);

      // If a previous soft shelve for the same window is somehow still
      // pending (re-entrant call), cancel its timer first — idempotent.
      const prev = this.softShelved.get(session.windowId);
      if (prev) {
        try { clearTimeout(prev.timer); } catch { /* best effort */ }
      }

      // Snapshot what reattach needs to rebuild the sessions map entry.
      const snapshot = {
        sessionName,
        wsPort: session.wsPort,
        displayName: session.displayName,
        titleLocked: session.titleLocked,
        daemonPid: session.daemonPid,
        speakMode: session.speakMode,
        softShelvedAt: Date.now(),
      };

      // Hide from the active sessions sidebar — webview tab disappears,
      // user can bring it back via the reattach picker (which now lists
      // soft entries) or by re-opening from Cmd+Shift+P.
      this.view?.webview.postMessage({
        type: 'remove-session',
        sessionName,
      });
      this.sessions.delete(sessionName);

      // Schedule TTL transition. When it fires, re-add to sessions map (so
      // the hard path can find the session by name) and call shelveSession
      // again with forceHard=true.
      const timer = setTimeout(() => {
        const soft = this.softShelved.get(snapshot.sessionName);
        if (!soft) return; // already reattached
        logger.info(`ImmorTerm AI: soft-shelve TTL expired for '${sessionName}' — escalating to hard shelve`);
        this.softShelved.delete(session.windowId!);
        // Re-register in active map briefly so hard shelve has the session
        // to act on. Hard shelve immediately removes it again.
        this.sessions.set(sessionName, {
          wsPort: snapshot.wsPort,
          displayName: snapshot.displayName,
          windowId: session.windowId!,
          titleLocked: snapshot.titleLocked,
          needsAttention: false,
          daemonPid: snapshot.daemonPid,
          ...(snapshot.speakMode ? { speakMode: snapshot.speakMode } : {}),
        });
        this.shelveSession(sessionName, true).catch(err => {
          logger.warn(`ImmorTerm AI: TTL hard-shelve for '${sessionName}' failed: ${err}`);
        });
      }, ttlMs);

      this.softShelved.set(session.windowId, { ...snapshot, timer });
      return;
    }

    logger.info(`ImmorTerm AI: shelving session '${sessionName}' (pid: ${session.daemonPid})`);

    // Memory digestion is handled by the Rust singleton daemon
    // (immorterm-digest). Its BurstQuiet debouncer fires automatically on
    // transcript write quiescence; no explicit shelve-time trigger needed.

    // 1b. Check for in-progress tasks linked to this session
    if (this.taskStorage && session.windowId) {
      try {
        const inProgressTasks = this.taskStorage.getInProgressForSession(session.windowId);
        if (inProgressTasks.length > 0) {
          const names = inProgressTasks.map(t => t.title).join(', ');
          const label = inProgressTasks.length === 1
            ? `Mark task "${inProgressTasks[0].title}" as done?`
            : `Mark ${inProgressTasks.length} tasks as done? (${names})`;
          vscode.window.showInformationMessage(label, 'Yes', 'Not yet').then(choice => {
            if (choice === 'Yes') {
              for (const t of inProgressTasks) {
                this.taskStorage?.update(t.id, { status: 'done' });
              }
            }
          });
        }
      } catch {
        // Non-fatal — task storage may not be initialized
      }
    }

    // 2. Clean up MCP gateway children (keyed by Claude's PID, not daemon PID)
    //    AND determine whether Claude was actually running at shelve time.
    //    If the user typed `/exit` (Claude gone before shelve), the registry
    //    can still hold a STALE claude_session_id from a previous run — and
    //    blindly using it on reattach would auto-resume a session the user
    //    deliberately ended. Capture liveness here so the post-SIGTERM
    //    bookkeeping below knows whether to keep or strip the UUID.
    let claudeAliveAtShelve = false;
    if (session.daemonPid) {
      try {
        const claudePid = await findClaudePidInTree(session.daemonPid);
        if (claudePid) {
          claudeAliveAtShelve = !!claudePid && isProcessAlive(claudePid);
          cleanupGatewaySessionByPid(claudePid);
          logger.info(`ImmorTerm AI: gateway cleanup for Claude pid ${claudePid} (alive: ${claudeAliveAtShelve})`);
        }
      } catch {
        // Gateway may not be running — non-fatal
      }
    }

    // 4. Kill daemon gracefully FIRST, then descendants.
    //    The daemon's SIGTERM handler flushes structured logs (grid.jsonl, cast, ai.jsonl)
    //    before exiting. If we kill everything at once, SIGKILL fires after 2s and
    //    can kill the daemon mid-flush — causing data loss.
    if (session.daemonPid) {
      try {
        // 4a. Snapshot descendants BEFORE killing daemon (they reparent to init once daemon dies)
        const descendants = await getDescendantPids(session.daemonPid);

        // 4b. SIGTERM the daemon only — give it up to 5s to flush and exit
        auditedKill(session.daemonPid, 'SIGTERM', 'shelveSession: graceful daemon shutdown');
        const deadline = Date.now() + 5000;
        while (Date.now() < deadline && isProcessAlive(session.daemonPid)) {
          await new Promise(r => setTimeout(r, 100));
        }
        if (isProcessAlive(session.daemonPid)) {
          logger.warn(`ImmorTerm AI: daemon ${session.daemonPid} didn't exit in 5s — sending SIGKILL`);
          auditedKill(session.daemonPid, 'SIGKILL', 'shelveSession: daemon SIGKILL after 5s');
        } else {
          logger.info(`ImmorTerm AI: daemon ${session.daemonPid} exited gracefully (logs flushed)`);
        }

        // 4c. Now kill remaining descendants (Claude, shell, etc.)
        if (descendants.length > 0) {
          logger.info(`ImmorTerm AI: killing ${descendants.length} descendant(s) for shelve`);
          await killDescendants(descendants);
        }
      } catch (err) {
        logger.warn(`ImmorTerm AI: process cleanup error during shelve: ${err}`);
      }
    }

    // 5. Mark as shelved — move entry from registry.json to registry-shelved.json
    // so registry.json only holds sessions the user has NOT deliberately closed.
    // session-status.json still tracks the shelved flag for quick filtering.
    //
    // Capture claude_session_id AFTER the daemon SIGTERM-flush so we read the
    // FINAL registry state. If we capture pre-SIGTERM (the original code), the
    // daemon's late update of claude_session_id (e.g. when a fresh tier-3
    // recall claude was running) lands in registry-shelved but session-status
    // gets the stale value — divergence then makes reattach pick the wrong
    // UUID. See SHELVE ME 2 / bc1ee994 vs 55295042 incident.
    //
    // ALSO: if claude was NOT alive at shelve time (user did `/exit` before
    // closing the tab), strip the stale claude_session_id from the registry
    // BEFORE the move — otherwise reattach will auto-resume a session the
    // user deliberately exited. The registry retains the field forever once
    // claude has run once; the daemon doesn't clear it on claude SIGCHLD.
    if (session.windowId) {
      if (!claudeAliveAtShelve) {
        try {
          const removed = removeClaudeSessionId(session.windowId);
          if (removed) {
            logger.info(`ImmorTerm AI: stripped stale claude_session_id at shelve (claude was not alive) for ${session.windowId}`);
          }
        } catch (err) {
          logger.warn(`ImmorTerm AI: removeClaudeSessionId failed: ${err}`);
        }
      }
      const claudeSessionId = claudeAliveAtShelve
        ? (getCurrentClaudeSessionId(session.windowId) || undefined)
        : undefined;
      updateSessionStatus(
        session.windowId,
        'shelved',
        Math.floor(Date.now() / 1000),
        claudeSessionId,
        !claudeAliveAtShelve,
      );
      try { moveToShelvedRegistry(session.windowId); } catch (err) {
        logger.warn(`ImmorTerm AI: moveToShelvedRegistry failed for ${session.windowId}: ${err}`);
      }
    }

    // 6. Archive session logs (move to archive/ — restored on reattach)
    if (this.projectPath && session.windowId) {
      try {
        const { archiveSessionByWindowId } = await import('./commands/cleanup');
        const logsDir = getLogsDir(this.projectPath);
        const archived = await archiveSessionByWindowId(logsDir, session.windowId);
        if (archived) {
          logger.info(`ImmorTerm AI: archived logs for session '${sessionName}'`);
        } else {
          logger.warn(`ImmorTerm AI: archive returned false for '${sessionName}' — session dir may not exist`);
        }
      } catch (err) {
        logger.warn(`ImmorTerm AI: failed to archive logs for shelve: ${err}`);
      }
    }

    // Clean up socket file (.ws port file)
    try {
      const files = fs.readdirSync(SOCKET_DIR);
      const wsFile = files.find(f => f.endsWith(`.${sessionName}.ws`));
      if (wsFile) {
        fs.unlinkSync(path.join(SOCKET_DIR, wsFile));
        logger.debug(`ImmorTerm AI: removed socket file ${wsFile}`);
      }
    } catch {
      // Socket dir may not exist — non-fatal
    }

    // 7. Remove from in-memory map (but NOT from registry — that's the shelved record)
    this.sessions.delete(sessionName);
    logger.info(`ImmorTerm AI: session '${sessionName}' shelved (registry entry preserved for reattach)`);
  }

  /**
   * Delete a shelved session permanently — remove from registry, delete archived logs.
   */
  private async deleteShelvedSession(windowId: string): Promise<void> {
    logger.info(`ImmorTerm AI: deleting shelved session ${windowId}`);

    // Remove from session-status + registry + shelved-registry.
    // updateSessionStatus writes a 'dead' tombstone in case any observer
    // is watching for the transition; removeSessionStatus then drops the
    // whole entry (including session_order) since this is a permanent delete.
    updateSessionStatus(windowId, 'dead');
    removeTerminalFromRegistry(windowId);
    removeSessionStatus(windowId);
    try { removeShelvedRegistryEntry(windowId); } catch { /* best effort */ }

    // Delete archived log directory
    if (this.projectPath) {
      try {
        const { findSessionDir } = await import('./commands/cleanup');
        const logsDir = getLogsDir(this.projectPath);
        const archiveDir = path.join(logsDir, 'archive');
        // Check both archive/ and logs/ — session may not have been archived yet
        for (const dir of [archiveDir, logsDir]) {
          const sessionDir = await findSessionDir(dir, windowId);
          if (sessionDir) {
            const fsP = await import('fs/promises');
            await fsP.rm(sessionDir, { recursive: true, force: true });
            logger.info(`ImmorTerm AI: deleted session logs: ${sessionDir}`);
          }
        }
      } catch (err) {
        logger.warn(`ImmorTerm AI: failed to delete logs for ${windowId}: ${err}`);
      }
    }

    logger.info(`ImmorTerm AI: shelved session ${windowId} deleted`);
  }

  /**
   * Reattach a shelved AI session — spawn fresh daemon, connect to webview.
   *
   * Called from the "Reattach Terminal" command when user picks an AI session.
   * Reads registry entry, unarchives logs, spawns daemon, adds to sessions map,
   * and tells the webview to create the tab + WebSocket connection.
   */
  public async reattachSession(windowId: string): Promise<boolean> {
    // Soft-shelve fast path: if this windowId is in the soft map AND the
    // daemon is still alive, just re-add to sessions and post add-session.
    // No SIGTERM, no archive, no claude --resume — pure WS reconnect.
    const soft = this.softShelved.get(windowId);
    if (soft) {
      const daemonStillAlive = !!soft.daemonPid && isProcessAlive(soft.daemonPid);
      if (daemonStillAlive) {
        logger.info(`ImmorTerm AI: SOFT reattach '${soft.sessionName}' (instant — daemon pid: ${soft.daemonPid})`);
        try { clearTimeout(soft.timer); } catch { /* best effort */ }
        this.softShelved.delete(windowId);
        const softBranch = detectGitBranch(this.projectPath);
        this.sessions.set(soft.sessionName, {
          wsPort: soft.wsPort,
          displayName: soft.displayName,
          windowId,
          titleLocked: soft.titleLocked,
          needsAttention: false,
          daemonPid: soft.daemonPid,
          projectDir: this.projectPath,
          branch: softBranch,
          ...(soft.speakMode ? { speakMode: soft.speakMode } : {}),
        });
        this.armSessionBranchWatcher(this.projectPath);
        this.view?.webview.postMessage({
          type: 'add-session',
          sessionName: soft.sessionName,
          wsPort: soft.wsPort,
          displayName: soft.displayName,
          titleLocked: soft.titleLocked,
          windowId,
          speakMode: soft.speakMode,
        });
        return true;
      }
      // Daemon died inside the soft TTL window (laptop sleep + sudden
      // kill, OOM, etc.) — drop the soft state and fall through to the
      // normal respawn path below.
      logger.warn(`ImmorTerm AI: soft-shelved daemon for ${windowId} no longer alive — falling back to respawn`);
      try { clearTimeout(soft.timer); } catch { /* best effort */ }
      this.softShelved.delete(windowId);
    }

    // Unshelve: move entry back from registry-shelved.json into registry.json.
    // Idempotent — no-op if already in live registry. Must run before
    // getRegistryEntryByWindowId since that only reads the live file.
    try { moveFromShelvedRegistry(windowId); } catch { /* best effort */ }

    const entry = getRegistryEntryByWindowId(windowId);
    if (!entry) {
      logger.warn(`ImmorTerm AI: reattach failed — no registry entry for ${windowId}`);
      return false;
    }

    const sessionName = entry.name;
    if (!sessionName) {
      logger.warn(`ImmorTerm AI: reattach failed — registry entry has no name for ${windowId}`);
      return false;
    }

    // Don't reattach if already in sessions map
    if (this.sessions.has(sessionName)) {
      logger.info(`ImmorTerm AI: session '${sessionName}' already active, switching to it`);
      this.view?.webview.postMessage({ type: 'switch-session', sessionName });
      return true;
    }

    logger.info(`ImmorTerm AI: reattaching shelved session '${sessionName}' (windowId: ${windowId})`);

    // 1. Read Claude resume ID BEFORE clearing shelved status (clearing deletes it).
    //    Prefer the registry-shelved entry's claude_session_id over
    //    session-status.json's claude_resume_id — the shelved entry is moved
    //    AFTER the daemon's graceful SIGTERM flush, so it reflects the final
    //    daemon state. session-status was captured pre-SIGTERM and can be
    //    stale if the daemon's claude_session_id update happened during
    //    shutdown. With the old order (session-status first), shelved
    //    sessions were resuming the WRONG (older) Claude UUID.
    //
    // Also: if shelve detected claude was NOT alive (`claude_explicitly_exited`
    // flag), force the resume ID empty AND signal NO_AUTO_RESUME to the daemon
    // so its recall cascade doesn't fall through to the claude-env mtime
    // fallback (which would resurrect a stale UUID from a prior claude run).
    const claudeWasExplicitlyExited = getClaudeExplicitlyExited(windowId);
    const claudeSessionId = claudeWasExplicitlyExited
      ? ''
      : (entry.claude_session_id || getClaudeResumeId(windowId) || '');
    const displayName = entry.display_name || sessionName;
    const titleLocked = entry.title_locked || false;

    // 2. Clear shelved status
    updateSessionStatus(windowId, 'active');

    // 3. Unarchive session logs — verify the session dir exists after
    if (this.projectPath) {
      try {
        const { unarchiveSessionDir, findSessionDir } = await import('./commands/cleanup');
        const logsDir = getLogsDir(this.projectPath);
        const unarchived = await unarchiveSessionDir(logsDir, windowId);
        if (unarchived) {
          logger.info(`ImmorTerm AI: unarchived logs for '${sessionName}' → ${unarchived}`);
        } else {
          // Check if it's already in logs/ (wasn't archived)
          const existingDir = await findSessionDir(logsDir, windowId);
          if (existingDir) {
            logger.info(`ImmorTerm AI: session dir already in logs/ for '${sessionName}' (was not archived)`);
          } else {
            logger.warn(`ImmorTerm AI: no session dir found for '${sessionName}' — scrollback will not be restored`);
          }
        }
      } catch (err) {
        logger.warn(`ImmorTerm AI: failed to unarchive logs: ${err}`);
      }
    }

    // 4. Resolve daemon binary
    const binary = await findDaemonBinary();
    if (!binary) {
      logger.warn('ImmorTerm AI: cannot reattach — daemon binary not found');
      return false;
    }

    // 5. Clean up stale .ws files and spawn fresh daemon
    cleanupStaleWsFiles(sessionName);

    const spawnedPid = await spawnDaemon(binary, sessionName, this.projectPath, windowId, displayName, claudeSessionId, titleLocked, claudeWasExplicitlyExited);
    const wsPort = await waitForWsPort(sessionName, 8000, spawnedPid);
    if (!wsPort) {
      logger.warn(`ImmorTerm AI: reattach daemon for '${sessionName}' did not start (no ws_port)`);
      return false;
    }

    const daemonPid = findDaemonPidFromWsFile(sessionName);
    logger.info(`ImmorTerm AI: reattached '${displayName}' (daemon pid: ${daemonPid}, ws: ${wsPort})`);

    // 6. Add to sessions map (rehydrate Speak Mode override from session-status.json)
    const speakMode = getSessionSpeakMode(windowId);
    const reattachBranch = detectGitBranch(this.projectPath);
    this.sessions.set(sessionName, {
      wsPort, displayName, windowId, titleLocked, needsAttention: false, daemonPid,
      projectDir: this.projectPath, branch: reattachBranch,
      ...(speakMode ? { speakMode } : {}),
    });
    this.armSessionBranchWatcher(this.projectPath);

    // 7. Tell webview to add the session tab + connect WebSocket
    this.view?.webview.postMessage({
      type: 'add-session',
      sessionName,
      wsPort,
      displayName,
      titleLocked,
      windowId,
      speakMode,
    });

    return true;
  }

  // ── Modal handler methods ─────────────────────────────────────

  private async runDiagnostics() {
    type Check = { name: string; status: 'pass' | 'warn' | 'fail'; detail: string };
    const checks: Check[] = [];

    // 1. ImmorTerm Binary
    try {
      const binary = await findDaemonBinary();
      if (binary) {
        const version = await new Promise<string>((resolve) => {
          execFile(binary, ['-v'], { timeout: 5000 }, (err, stdout) => {
            resolve(err ? 'Found' : stdout.trim() || 'Found');
          });
        });
        checks.push({ name: 'ImmorTerm Binary', status: 'pass', detail: version });
      } else {
        checks.push({ name: 'ImmorTerm Binary', status: 'fail', detail: 'Not found' });
      }
    } catch {
      checks.push({ name: 'ImmorTerm Binary', status: 'fail', detail: 'Error checking binary' });
    }

    // 2. Memory Service (native Rust binary)
    try {
      const memState = getOpenMemoryState();
      checks.push({
        name: 'Memory Service',
        status: memState.apiHealthy ? 'pass' : memState.stackRunning ? 'warn' : 'fail',
        detail: memState.apiHealthy ? 'Healthy' : memState.stackRunning ? 'Starting...' : 'Not running',
      });
    } catch {
      checks.push({ name: 'Memory Service', status: 'fail', detail: 'Error' });
    }

    // 3. MCP Gateway
    try {
      const gwState = getMCPGatewayState();
      checks.push({
        name: 'MCP Gateway',
        status: gwState.healthy ? 'pass' : gwState.running ? 'warn' : 'fail',
        detail: gwState.healthy ? 'Healthy' : gwState.running ? 'Running (unhealthy)' : 'Not running',
      });
    } catch {
      checks.push({ name: 'MCP Gateway', status: 'fail', detail: 'Error' });
    }

    // 7. License
    try {
      const isPro = isProTier();
      const tier = getLicenseStatus().tier;
      checks.push({
        name: 'License',
        status: isPro ? 'pass' : 'warn',
        detail: isPro ? (tier === 'memory-pro' ? 'Memory Pro' : 'Pro') : 'Free',
      });
    } catch {
      checks.push({ name: 'License', status: 'warn', detail: 'Unknown' });
    }

    // 8. Active Sessions
    checks.push({
      name: 'Active Sessions',
      status: this.sessions.size > 0 ? 'pass' : 'warn',
      detail: `${this.sessions.size} session(s)`,
    });

    this.view?.webview.postMessage({ type: 'diagnostics-result', checks });
  }

  private async sendSessionSummary(sessionName: string) {
    try {
      const session = this.sessions.get(sessionName);
      const windowId = session?.windowId;
      if (!windowId) {
        this.view?.webview.postMessage({ type: 'session-summary-result', data: { error: 'Session not found' } });
        return;
      }
      await this.showSessionSummary(windowId);
    } catch (err: any) {
      this.view?.webview.postMessage({
        type: 'session-summary-result',
        data: { error: err.message || 'Failed to fetch session summary' },
      });
    }
  }

  /**
   * Collect everything we know about a session from registry, daemon
   * process tree, file system, and the in-memory provider state. Used by
   * the right-click "Session Info" modal — answers questions like "is this
   * a Claude session? what's the UUID? where do logs live? is the daemon
   * actually alive?" without needing to grep multiple files manually.
   */
  private async collectSessionInfo(
    windowId: string,
    sessionName?: string,
  ): Promise<Record<string, unknown>> {
    const fs = await import('fs');
    const fsP = await import('fs/promises');
    const path = await import('path');
    const { promisify } = await import('util');
    const { execFile } = await import('child_process');
    const execFileP = promisify(execFile);

    const liveSession = sessionName ? this.sessions.get(sessionName) : undefined;
    const soft = this.softShelved.get(windowId);

    // Registry — try live first, then shelved fallback.
    let regEntry = getRegistryEntryByWindowId(windowId);
    let registrySource: 'live' | 'shelved' | 'none' = 'live';
    if (!regEntry) {
      try {
        const allShelved = getShelvedSessions();
        const found = allShelved.find(e => e.window_id === windowId);
        if (found) { regEntry = found; registrySource = 'shelved'; }
      } catch { /* best effort */ }
    }
    if (!regEntry) registrySource = 'none';

    // Daemon process state. Prefer live, then soft-shelved, then registry pid.
    const daemonPid = liveSession?.daemonPid ?? soft?.daemonPid ?? regEntry?.pid;
    const daemonAlive = !!daemonPid && daemonPid > 0 && isProcessAlive(daemonPid);

    // Walk the descendant tree using the existing safe helper, then resolve
    // PIDs → cmd names via a single execFile (no shell) ps call.
    let descendants: Array<{ pid: number; cmd: string }> = [];
    if (daemonAlive && daemonPid) {
      try {
        const childPids = await getDescendantPids(daemonPid);
        if (childPids.length > 0) {
          const args = ['-o', 'pid=,command=', '-p', childPids.join(',')];
          const { stdout } = await execFileP('ps', args, { timeout: 3000 });
          for (const line of stdout.split('\n')) {
            const m = line.trim().match(/^(\d+)\s+(.*)$/);
            if (m) descendants.push({ pid: parseInt(m[1], 10), cmd: m[2] });
          }
        }
      } catch { /* best effort */ }
    }

    // Detect "currently active vendor" from descendant cmd names. This
    // bypasses the (chronically racy) registry.claude_session_id field —
    // if claude is in the process tree, we know it's running NOW even when
    // registry says null.
    const vendorMatch = (cmd: string): string | null => {
      const c = cmd.toLowerCase();
      if (c.includes('claude')) return 'claude';
      if (c.includes('cursor')) return 'cursor';
      if (c.includes('aider')) return 'aider';
      if (c.includes('codex')) return 'codex';
      return null;
    };
    const activeVendor = descendants
      .map(d => vendorMatch(d.cmd))
      .find(Boolean) || null;

    // Resolve structured log dir + status (live vs archived).
    let structuredLogDir: string | undefined = regEntry?.structured_log_dir;
    let logDirStatus: 'live' | 'archived' | 'missing' = 'missing';
    if (structuredLogDir) {
      if (fs.existsSync(structuredLogDir)) {
        logDirStatus = 'live';
      } else {
        const parent = path.dirname(structuredLogDir);
        const archived = path.join(parent, 'archive', path.basename(structuredLogDir));
        if (fs.existsSync(archived)) {
          structuredLogDir = archived;
          logDirStatus = 'archived';
        }
      }
    }

    const fileStat = async (p: string) => {
      try {
        const s = await fsP.stat(p);
        return { bytes: s.size, mtime: s.mtime.toISOString() };
      } catch { return null; }
    };
    const dirFiles: Record<string, unknown> = {};
    if (structuredLogDir && logDirStatus !== 'missing') {
      for (const name of ['grid.jsonl', 'ai.jsonl', 'cast', 'session.json', 'death.json', 'daemon.log']) {
        dirFiles[name] = await fileStat(path.join(structuredLogDir, name));
      }
    }

    // ai.jsonl summary — best signal of whether the daemon's AI detector
    // ever fired. Empty file = detector never saw the AI tool.
    let aiJsonlInfo: Record<string, unknown> | null = null;
    if (structuredLogDir && (dirFiles['ai.jsonl'] as { bytes?: number } | null)?.bytes) {
      try {
        const raw = await fsP.readFile(path.join(structuredLogDir, 'ai.jsonl'), 'utf-8');
        const lines = raw.split('\n').filter(l => l.trim());
        const lastLine = lines[lines.length - 1];
        const lastEvent = lastLine ? JSON.parse(lastLine) : null;
        const eventCounts: Record<string, number> = {};
        for (const line of lines) {
          try {
            const e = JSON.parse(line);
            const ev = e.event || 'unknown';
            eventCounts[ev] = (eventCounts[ev] || 0) + 1;
          } catch { /* skip malformed */ }
        }
        aiJsonlInfo = {
          totalLines: lines.length,
          eventCounts,
          lastEvent: lastEvent
            ? {
                event: lastEvent.event,
                role: lastEvent.role,
                tool: lastEvent.tool,
                ts: lastEvent.ts,
                contentPreview: typeof lastEvent.content === 'string'
                  ? lastEvent.content.slice(0, 120) : undefined,
              }
            : null,
        };
      } catch (err) {
        aiJsonlInfo = { error: `Failed to parse: ${err}` };
      }
    }

    const softInfo = soft ? {
      softShelvedAt: new Date(soft.softShelvedAt).toISOString(),
      ttlElapsedMs: Date.now() - soft.softShelvedAt,
    } : null;

    // session-status.json breadcrumb (claude_resume_id may be stale —
    // surface it explicitly so user can spot drift vs registry).
    let sessionStatus: Record<string, unknown> | null = null;
    try {
      const sst = path.join(os.homedir(), '.immorterm', 'session-status.json');
      if (fs.existsSync(sst)) {
        const data = JSON.parse(fs.readFileSync(sst, 'utf-8'));
        sessionStatus = data.sessions?.[windowId] || null;
      }
    } catch { /* best effort */ }

    return {
      windowId,
      immortermId: windowId,
      sessionName: sessionName || regEntry?.name || null,
      displayName: liveSession?.displayName || regEntry?.display_name || null,
      sessionType: regEntry?.session_type || null,
      projectDir: regEntry?.project_dir || null,
      titleLocked: liveSession?.titleLocked ?? regEntry?.title_locked ?? false,
      activeVendor,
      registryClaudeSessionId: regEntry?.claude_session_id || null,
      sessionStatusClaudeResumeId: (sessionStatus as { claude_resume_id?: string } | null)?.claude_resume_id || null,
      claudeStats: regEntry?.claude_stats || null,
      registrySource,
      logDirStatus,
      isSoftShelved: !!soft,
      softInfo,
      sessionStatus,
      daemonPid: daemonPid || null,
      daemonAlive,
      descendants,
      wsPort: liveSession?.wsPort || regEntry?.ws_port || null,
      structuredLogDir: structuredLogDir || null,
      files: dirFiles,
      aiJsonlInfo,
      speakMode: liveSession?.speakMode || null,
      theme: regEntry?.theme || null,
    };
  }

  /**
   * Show session summary modal in the webview for any windowId (active or shelved).
   * Focuses the ImmorTerm panel, fetches session context from memory API, and posts to webview.
   */
  public async showSessionSummary(windowId: string): Promise<void> {
    // Focus the panel so the modal is visible
    await vscode.commands.executeCommand('immorterm.terminalView.focus');
    try {
      const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
      const userId = getStableProjectId(this.projectPath);
      const url = `${getMemoryUrl()}/api/v1/sessions/context?immorterm_id=${encodeURIComponent(windowId)}&user_id=${encodeURIComponent(userId)}`;
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 5000);
      const res = await fetch(url, { signal: controller.signal });
      clearTimeout(timeout);
      const data = await res.json();
      this.view?.webview.postMessage({ type: 'session-summary-result', data });
    } catch (err: any) {
      this.view?.webview.postMessage({
        type: 'session-summary-result',
        data: { error: err.message || 'Failed to fetch session summary' },
      });
    }
  }

  /**
   * Silent fetch of session title + at_a_glance for the session-switch
   * popover. Same API as showSessionSummary() but does NOT focus the
   * panel or open the modal — just posts the minimal fields back so the
   * popover can update in place.
   */
  private async fetchSessionSwitchSummary(windowId: string): Promise<void> {
    const lastUserPrompt = this.readLastUserPromptFromAiLog(windowId);
    try {
      const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
      const userId = getStableProjectId(this.projectPath);
      const url = `${getMemoryUrl()}/api/v1/sessions/context?immorterm_id=${encodeURIComponent(windowId)}&user_id=${encodeURIComponent(userId)}`;
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 3000);
      const res = await fetch(url, { signal: controller.signal });
      clearTimeout(timeout);
      const data: any = await res.json();
      this.view?.webview.postMessage({
        type: 'session-switch-summary',
        windowId,
        title: data?.title || null,
        at_a_glance: Array.isArray(data?.at_a_glance) ? data.at_a_glance : [],
        last_user_prompt: lastUserPrompt,
      });
    } catch {
      this.view?.webview.postMessage({
        type: 'session-switch-summary',
        windowId,
        title: null,
        at_a_glance: [],
        last_user_prompt: lastUserPrompt,
      });
    }
  }

  /**
   * Tails the session's ai.jsonl to extract the most recent user turn.
   * Returns a short, single-line snippet suitable for the hover popover,
   * or null if the log cannot be located or contains no user turns.
   */
  private readLastUserPromptFromAiLog(windowId: string): string | null {
    const candidates = [
      path.join(this.projectPath, '.immorterm', 'terminals', 'logs'),
      path.join(os.homedir(), '.immorterm', 'terminals', 'logs'),
    ];
    let aiPath: string | null = null;
    for (const base of candidates) {
      try {
        const entries = fs.readdirSync(base);
        // Match bare windowId (new format) or legacy `{date}_{windowId}`.
        const match = entries.find((e) => e === windowId || e.endsWith(`_${windowId}`));
        if (!match) continue;
        const candidate = path.join(base, match, 'ai.jsonl');
        if (fs.existsSync(candidate)) {
          aiPath = candidate;
          break;
        }
      } catch {
        continue;
      }
    }
    if (!aiPath) return null;

    try {
      const stat = fs.statSync(aiPath);
      const readSize = Math.min(stat.size, 131072); // tail last 128 KB
      const fd = fs.openSync(aiPath, 'r');
      try {
        const buf = Buffer.alloc(readSize);
        fs.readSync(fd, buf, 0, readSize, stat.size - readSize);
        const text = buf.toString('utf8');
        // Skip potentially-partial first line when we didn't start at offset 0
        const firstNl = readSize < stat.size ? text.indexOf('\n') : -1;
        const lines = text.slice(firstNl + 1).split('\n');
        // Walk backwards, but only accept a user turn if we've already seen
        // an assistant turn after it — otherwise it's likely still being typed
        // (the screen-capture extractor emits evolving user rows per keystroke).
        let seenAssistantAfter = false;
        for (let i = lines.length - 1; i >= 0; i--) {
          const line = lines[i];
          if (!line) continue;
          try {
            const entry = JSON.parse(line);
            if (entry.event !== 'turn') continue;
            if (entry.role === 'assistant') {
              seenAssistantAfter = true;
              continue;
            }
            if (entry.role !== 'user') continue;
            if (!seenAssistantAfter) continue; // prompt not yet replied to
            const content: string = typeof entry.content === 'string' ? entry.content : '';
            const cleaned = content
              .replace(/\\n/g, '\n') // normalize literal backslash-n from screen captures
              .replace(/^\s*❯\s*/, '')
              .split('\n')[0]
              .trim();
            if (!cleaned) continue;
            // Skip separator-only snapshots (empty prompt row, divider chars, etc.)
            if (/^[─━—–_=\-\s]+$/.test(cleaned)) continue;
            // Skip permission-dialog numeric selections (e.g. "1. Yes", "2. No")
            if (/^\d+\.\s+(Yes|No)\b/i.test(cleaned)) continue;
            return cleaned.length > 240 ? cleaned.slice(0, 237) + '...' : cleaned;
          } catch {
            continue;
          }
        }
      } finally {
        fs.closeSync(fd);
      }
    } catch {
      return null;
    }
    return null;
  }

  /** Cache of session_id -> title for enriching search results. */
  private sessionTitleCache = new Map<string, string>();

  private async searchMemories(query: string, limit: number, offset: number, scopeAll: boolean): Promise<void> {
    try {
      const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
      const memUrl = getMemoryUrl();
      const userId = getStableProjectId(this.projectPath);
      const params = new URLSearchParams({
        query,
        user_id: userId,
        limit: String(limit + offset),
        scope: scopeAll ? 'all' : 'session',
      });
      if (!scopeAll && this.sessions.size > 0) {
        const firstSession = this.sessions.values().next().value;
        if (firstSession) {
          params.set('immorterm_id', firstSession.windowId);
        }
      }
      const url = `${memUrl}/api/v1/memories/search?${params}`;
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 5000);
      const t0 = performance.now();
      const res = await fetch(url, { signal: controller.signal });
      clearTimeout(timeout);
      const elapsed = Math.round(performance.now() - t0);
      const data = await res.json() as { results?: Array<Record<string, unknown>>; count?: number; elapsed_ms?: number };
      if (offset > 0 && data.results) {
        data.results = data.results.slice(offset);
      }
      data.elapsed_ms = elapsed;

      // Inject cached session titles immediately
      if (data.results) {
        for (const r of data.results) {
          const sid = (r.session_id || (r.metadata as Record<string, unknown>)?.session_id) as string | undefined;
          if (sid && this.sessionTitleCache.has(sid)) {
            r.session_title = this.sessionTitleCache.get(sid);
          }
        }
      }

      // Send results immediately — don't block on title lookup
      this.view?.webview.postMessage({ type: 'memory-search-result', data });

      // Async title enrichment: fetch missing session titles and re-send
      if (data.results) {
        const missingSids = new Set<string>();
        for (const r of data.results) {
          const sid = (r.session_id || (r.metadata as Record<string, unknown>)?.session_id) as string | undefined;
          if (sid && !this.sessionTitleCache.has(sid)) missingSids.add(sid);
        }
        if (missingSids.size > 0) {
          this.enrichSessionTitles(missingSids, data).catch(() => {});
        }
      }
    } catch (err: any) {
      const msg = err.message?.includes('ECONNREFUSED') || err.message?.includes('abort')
        ? 'Memory service unavailable. Run `immorterm serve` to start.'
        : (err.message || 'Search failed');
      this.view?.webview.postMessage({
        type: 'memory-search-result',
        data: { error: msg },
      });
    }
  }

  private async enrichSessionTitles(_sids: Set<string>, data: { results?: Array<Record<string, unknown>> }): Promise<void> {
    const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
    const memUrl = getMemoryUrl();
    const userId = getStableProjectId(this.projectPath);
    const params = new URLSearchParams({ user_id: userId, limit: '50', hours_ago: '8760' });
    const res = await fetch(`${memUrl}/api/v1/sessions?${params}`, {
      signal: AbortSignal.timeout(3000),
    });
    const { sessions } = await res.json() as { sessions?: Array<{ session_id?: string; title?: string }> };
    if (!sessions) return;
    for (const s of sessions) {
      if (s.session_id && s.title) this.sessionTitleCache.set(s.session_id, s.title);
    }
    // Re-inject titles into results and send updated data
    let updated = false;
    if (data.results) {
      for (const r of data.results) {
        const sid = (r.session_id || (r.metadata as Record<string, unknown>)?.session_id) as string | undefined;
        if (sid && !r.session_title && this.sessionTitleCache.has(sid)) {
          r.session_title = this.sessionTitleCache.get(sid);
          updated = true;
        }
      }
    }
    if (updated) {
      this.view?.webview.postMessage({ type: 'memory-search-result', data });
    }
  }

  private async sendServiceStatus() {
    const services = [];

    // Memory service
    const memEnabled = isMemoryEnabled();
    let memHealthy = false;
    try {
      if (memEnabled) memHealthy = await checkOpenMemoryHealth();
    } catch { /* ignore */ }
    services.push({
      id: 'memory', name: 'ImmorTerm Memory',
      desc: 'Persistent AI memory — remembers decisions, context, and lessons across sessions',
      enabled: memEnabled, healthy: memHealthy, canStartStop: true,
      hasGraph: true, graphEnabled: isGraphEnabled(),
    });

    // Gateway service
    let gwHealthy = false;
    try {
      gwHealthy = await checkGatewayHealth();
    } catch { /* ignore */ }
    services.push({
      id: 'gateway', name: 'MCP Gateway',
      desc: 'Shared MCP server proxy — reduces memory ~90% by deduplicating tool processes',
      enabled: gwHealthy || getMCPGatewayState().running,
      healthy: gwHealthy, canStartStop: true,
      hasDashboard: true,
    });

    this.view?.webview.postMessage({ type: 'service-status', services });
  }

  private async handleServiceAction(serviceId: string, action: string) {
    try {
      if (serviceId === 'memory') {
        if (action === 'start') await startOpenMemory();
        else if (action === 'stop') await stopOpenMemory();
        else if (action === 'restart') { await stopOpenMemory(); await startOpenMemory(); }
      } else if (serviceId === 'gateway') {
        if (action === 'start') await startGateway();
        else if (action === 'stop') await stopGateway();
        else if (action === 'restart') { await stopGateway(); await startGateway(); }
        else if (action === 'dashboard') {
          const { openGatewayDashboard } = await import('./services/mcp-gateway');
          openGatewayDashboard();
        }
      }
    } catch (e) {
      logger.error(`ImmorTerm AI: service action failed: ${e}`);
    }
    this.view?.webview.postMessage({ type: 'service-action-result' });
  }

  private handleServiceToggle(serviceId: string, enabled: boolean) {
    const projectId = getStableProjectId(this.projectPath);
    if (serviceId === 'memory:graph') {
      setServiceEnabled(this.projectPath, 'graph', enabled, projectId);
    } else if (serviceId === 'memory') {
      setServiceEnabled(this.projectPath, 'memory', enabled, projectId);
    } else if (serviceId === 'gateway') {
      setServiceEnabled(this.projectPath, 'mcpGateway', enabled, projectId);
    }
    // Re-send status so the modal refreshes
    this.sendServiceStatus();
  }

  private sendLicenseStatus() {
    const lic = getLicenseStatus();
    const isPro = isProTier();
    this.view?.webview.postMessage({
      type: 'license-status',
      license: {
        isPro,
        key: lic.key,
        email: lic.customerEmail,
        expiresAt: lic.expiresAt,
        status: lic.status,
        tier: lic.tier,
      },
    });
  }

  private handleLicenseActivate(key: string) {
    try {
      const config = readGlobalConfig();
      config.license.key = key;
      config.license.status = 'pending';
      writeGlobalConfig(config);
      // Trigger validation via CLI in background
      spawn('npx', ['immorterm', 'license', 'validate'], {
        stdio: 'ignore', detached: true,
      }).unref();
      this.view?.webview.postMessage({ type: 'license-action-result', success: true });
    } catch (e) {
      this.view?.webview.postMessage({
        type: 'license-action-result', success: false, error: String(e),
      });
    }
  }

  private handleLicenseDeactivate() {
    try {
      const config = readGlobalConfig();
      config.license.key = null;
      config.license.instanceId = null;
      config.license.status = null;
      config.license.tier = null;
      config.license.expiresAt = null;
      config.license.lastValidatedAt = null;
      config.license.customerEmail = null;
      writeGlobalConfig(config);
      this.view?.webview.postMessage({ type: 'license-action-result', success: true });
    } catch (e) {
      this.view?.webview.postMessage({
        type: 'license-action-result', success: false, error: String(e),
      });
    }
  }

  private handleLicenseValidate() {
    spawn('npx', ['immorterm', 'license', 'validate'], {
      stdio: 'ignore', detached: true,
    }).unref();
    // Send current status immediately; actual validation runs async
    setTimeout(() => this.sendLicenseStatus(), 2000);
  }

  private async sendInsights(sessionName?: string) {
    try {
      const { getMemoryUrl } = await import('./services/memory/native-memory-manager');
      const userId = getStableProjectId(this.projectPath);
      let url = `${getMemoryUrl()}/api/v1/stats/insights?user_id=${encodeURIComponent(userId)}`;
      // Session-specific filtering when accessed from a terminal context
      if (sessionName) {
        const session = this.sessions.get(sessionName);
        if (session?.windowId) {
          url += `&immorterm_id=${encodeURIComponent(session.windowId)}`;
        }
      }
      const controller = new AbortController();
      const timeout = setTimeout(() => controller.abort(), 5000);
      const res = await fetch(url, { signal: controller.signal });
      clearTimeout(timeout);
      const data = await res.json();
      this.view?.webview.postMessage({ type: 'insights-result', data });
    } catch (e) {
      this.view?.webview.postMessage({ type: 'insights-result', data: { error: String(e) } });
    }
  }

  private async sendSessionLogs() {
    const logsDir = getLogsDir(this.projectPath);
    const sessions: Array<{
      name: string; alive: boolean; types: string[]; age: string; size: string;
    }> = [];

    try {
      const fsP = fs.promises;
      const entries = await fsP.readdir(logsDir).catch(() => [] as string[]);
      // Group files by session name (remove extensions like .grid.jsonl, .cast, .ai.jsonl)
      const sessionMap = new Map<string, { types: Set<string>; mtime: number; totalSize: number }>();
      for (const entry of entries) {
        const match = entry.match(/^(.+?)\.(grid\.jsonl|cast|ai\.jsonl|log)$/);
        if (!match) continue;
        const [, name, ext] = match;
        const type = ext === 'grid.jsonl' ? 'grid' : ext === 'ai.jsonl' ? 'ai' : ext;
        if (!sessionMap.has(name)) {
          sessionMap.set(name, { types: new Set(), mtime: 0, totalSize: 0 });
        }
        const info = sessionMap.get(name)!;
        info.types.add(type);
        try {
          const stat = await fsP.stat(path.join(logsDir, entry));
          info.mtime = Math.max(info.mtime, stat.mtimeMs);
          info.totalSize += stat.size;
        } catch { /* ignore stat errors */ }
      }

      // Check if session daemons are alive
      const socketEntries = await fsP.readdir(SOCKET_DIR).catch(() => [] as string[]);
      const aliveSet = new Set(socketEntries.map(s => s.replace(/\.sock$/, '')));

      for (const [name, info] of sessionMap) {
        const ageMins = Math.floor((Date.now() - info.mtime) / 60000);
        let age: string;
        if (ageMins < 60) age = `${ageMins}m ago`;
        else if (ageMins < 1440) age = `${Math.floor(ageMins / 60)}h ago`;
        else age = `${Math.floor(ageMins / 1440)}d ago`;

        let size: string;
        if (info.totalSize < 1024) size = `${info.totalSize}B`;
        else if (info.totalSize < 1048576) size = `${Math.floor(info.totalSize / 1024)}KB`;
        else size = `${(info.totalSize / 1048576).toFixed(1)}MB`;

        sessions.push({
          name, alive: aliveSet.has(name),
          types: [...info.types], age, size,
        });
      }

      // Sort: alive first, then by mtime descending
      sessions.sort((a, b) => {
        if (a.alive !== b.alive) return a.alive ? -1 : 1;
        return 0; // mtime is already in map order
      });
    } catch (e) {
      logger.error(`ImmorTerm AI: error scanning logs: ${e}`);
    }

    this.view?.webview.postMessage({ type: 'session-logs', sessions });
  }

  /** Find the task-state file matching the given immorterm_id (or session windowId). */
  private findTaskFile(immortermId?: string): string | null {
    const taskStateDir = path.join(os.homedir(), '.immorterm', 'task-state');
    const id = immortermId || [...this.sessions.values()].find((s: any) => s.windowId)?.windowId;
    if (!id) return null;
    try {
      const files = fs.readdirSync(taskStateDir).filter(f => f.startsWith('tasks-') && f.endsWith('.json'));
      for (const f of files) {
        try {
          const data = JSON.parse(fs.readFileSync(path.join(taskStateDir, f), 'utf8'));
          if (data.immorterm_id === id || data.session_id === id
              || (data.immorterm_id && id.endsWith(data.immorterm_id))
              || (id && data.immorterm_id?.endsWith(id))) {
            return path.join(taskStateDir, f);
          }
        } catch { /* skip */ }
      }
    } catch { /* skip */ }
    return null;
  }

  private searchTaskInFile(filePath: string, subjectLower: string): any {
    try {
      const data = JSON.parse(fs.readFileSync(filePath, 'utf8'));
      const tasks = data.tasks || {};
      for (const tid of Object.keys(tasks)) {
        const t = tasks[tid];
        const tSubject = (t.subject || '').toLowerCase().trim();
        if (tSubject === subjectLower || subjectLower.startsWith(tSubject) || tSubject.includes(subjectLower)) {
          return t;
        }
      }
    } catch { /* skip */ }
    return null;
  }

  private lookupTask(subject: string, requestId: string, immortermId?: string) {
    const subjectLower = subject.toLowerCase().trim();
    const taskStateDir = path.join(os.homedir(), '.immorterm', 'task-state');

    // 1. Try current session first
    const taskFile = this.findTaskFile(immortermId);
    let found = taskFile ? this.searchTaskInFile(taskFile, subjectLower) : null;

    // 2. Fall back to all files (cross-session tasks)
    if (!found) {
      try {
        const files = fs.readdirSync(taskStateDir).filter(f => f.startsWith('tasks-') && f.endsWith('.json'));
        for (const f of files) {
          const fp = path.join(taskStateDir, f);
          if (fp === taskFile) continue; // already searched
          found = this.searchTaskInFile(fp, subjectLower);
          if (found) break;
        }
      } catch { /* skip */ }
    }

    if (found) {
      this.view?.webview.postMessage({
        type: 'task-preview-result', requestId,
        task: { id: found.id, subject: found.subject, description: found.description, status: found.status,
                activeForm: found.activeForm, created_at: found.created_at, updated_at: found.updated_at,
                owner: found.owner, metadata: found.metadata, blockedBy: found.blockedBy, blocks: found.blocks },
      });
    } else {
      this.view?.webview.postMessage({ type: 'task-preview-result', requestId, task: null });
    }
  }

  private lookupAllTasks(requestId: string, immortermId?: string) {
    const taskFile = this.findTaskFile(immortermId);
    const allTasks: Array<{id: string; subject: string; status: string; description?: string}> = [];
    if (taskFile) {
      try {
        const data = JSON.parse(fs.readFileSync(taskFile, 'utf8'));
        const tasks = data.tasks || {};
        for (const tid of Object.keys(tasks)) {
          const t = tasks[tid];
          allTasks.push({ id: t.id, subject: t.subject, status: t.status, description: t.description });
        }
      } catch { /* skip */ }
    }
    const statusOrder: Record<string, number> = { in_progress: 0, pending: 1, completed: 2 };
    allTasks.sort((a, b) => (statusOrder[a.status] ?? 1) - (statusOrder[b.status] ?? 1) || Number(a.id) - Number(b.id));
    this.view?.webview.postMessage({ type: 'task-summary-result', requestId, tasks: allTasks });
  }

  private addImmorTermTask(subject: string, description: string, status: string) {
    // Find an active session's WS to send the create-task command
    const session = this.sessions?.values().next().value as { ws?: { send: (d: string) => void }; connected?: boolean } | undefined;
    if (session?.ws && session.connected) {
      session.ws.send(JSON.stringify({
        type: 'create_task', text: subject, description, status,
      }));
      vscode.window.showInformationMessage(`Task added to ImmorTerm: ${subject}`);
    } else {
      vscode.window.showWarningMessage('No active ImmorTerm session to add task to');
    }
  }

  private openSessionLog(sessionName: string, logType: string) {
    const logsDir = getLogsDir(this.projectPath);
    const ext = logType === 'grid' ? '.grid.jsonl' : logType === 'ai' ? '.ai.jsonl' : `.${logType}`;
    const logPath = path.join(logsDir, sessionName + ext);
    if (fs.existsSync(logPath)) {
      vscode.window.showTextDocument(vscode.Uri.file(logPath), { preview: true });
    } else {
      vscode.window.showWarningMessage(`Log file not found: ${sessionName}${ext}`);
    }
  }

  private async handleWizardCheck(step: string) {
    if (step === 'binary') {
      const binary = await findDaemonBinary();
      let version = '';
      let binaryPath = '';
      if (binary) {
        binaryPath = binary;
        version = await new Promise<string>((resolve) => {
          execFile(binary, ['-v'], { timeout: 5000 }, (err, stdout) => {
            resolve(err ? '' : stdout.trim());
          });
        });
      }
      this.view?.webview.postMessage({
        type: 'wizard-check-result', step,
        result: { found: !!binary, version, path: binaryPath },
      });
    } else if (step === 'memory') {
      const memState = getOpenMemoryState();
      const status = memState.apiHealthy ? 'running' : memState.stackRunning ? 'starting' : 'missing';
      this.view?.webview.postMessage({
        type: 'wizard-check-result', step, result: { status },
      });
    } else if (step === 'services') {
      const services = [];
      const memEnabled = isMemoryEnabled();
      let memHealthy = false;
      try { if (memEnabled) memHealthy = await checkOpenMemoryHealth(); } catch { /* ignore */ }
      services.push({ name: 'ImmorTerm Memory', enabled: memEnabled, healthy: memHealthy });

      let gwHealthy = false;
      try { gwHealthy = await checkGatewayHealth(); } catch { /* ignore */ }
      const gwRunning = getMCPGatewayState().running;
      services.push({ name: 'MCP Gateway', enabled: gwRunning || gwHealthy, healthy: gwHealthy });

      this.view?.webview.postMessage({
        type: 'wizard-check-result', step, result: { services },
      });
    }
  }

  private async handleWizardAction(action: string) {
    if (action === 'start-memory') {
      try { await startOpenMemory(); } catch { /* ignore */ }
    }
    this.view?.webview.postMessage({ type: 'wizard-action-result' });
  }

  dispose() {
    // Only clean up JS-side resources. Do NOT kill daemon sessions —
    // they are designed to outlive the extension (survive reload/restart).
    // Daemons are only killed when the user explicitly closes a session (X button).
    this.sessions.clear();
    this.disposables.forEach(d => d.dispose());
    this.disposables = [];
    this.stopAllGitWatchers();
  }

  /** Resolve the actual `.git` dir for a project path, handling worktree
   *  pointer files (`.git` is a regular file with `gitdir: <abs path>`). */
  private resolveGitDir(projectDir: string): string | null {
    try {
      let gitDir = path.join(projectDir, '.git');
      const st = fs.statSync(gitDir);
      if (st.isFile()) {
        const m = fs.readFileSync(gitDir, 'utf8').trim().match(/^gitdir:\s*(.+)$/);
        if (!m) return null;
        gitDir = path.isAbsolute(m[1]) ? m[1] : path.resolve(projectDir, m[1]);
      }
      return gitDir;
    } catch { return null; }
  }

  /** No-op since branch is now daemon-pushed via WS control events.
   *  The webview tracks `sessionBranches` per session and composes the
   *  status-bar label itself on session switch. Kept as a stub so callers
   *  in the restore + addSession paths don't need editing — the function
   *  just records the project_dir and returns; daemon emits drive the UI.
   *  This is host-agnostic (works in Tauri standalone too).
   */
  private armSessionBranchWatcher(_projectDir: string): void {
    return;
  }

  private armSessionBranchWatcher_DISABLED(projectDir: string): void {
    if (!projectDir) return;
    if (this.gitWatchers.has(projectDir)) return;

    const arm = (): void => {
      const gitDir = this.resolveGitDir(projectDir);
      if (!gitDir) return;
      if (!fs.existsSync(path.join(gitDir, 'HEAD'))) return;

      let debounce: NodeJS.Timeout | null = null;
      const fire = () => {
        if (debounce) clearTimeout(debounce);
        debounce = setTimeout(() => {
          const newBranch = detectGitBranch(projectDir);
          // Update every session sharing this project_dir.
          let activeAffected = false;
          for (const [name, s] of this.sessions) {
            if (s.projectDir !== projectDir) continue;
            if (s.branch === newBranch) continue;
            s.branch = newBranch;
            if (this.activeWindowId && s.windowId === this.activeWindowId) {
              activeAffected = true;
            }
            logger.info(`ImmorTerm AI: branch for '${name}' → '${newBranch || 'detached/none'}'`);
          }
          if (activeAffected) this.pushActiveBranchToWebview();
        }, 150);
      };

      try {
        const w = fs.watch(gitDir, (_eventType, filename) => {
          if (filename && filename !== 'HEAD') return;
          fire();
        });
        w.on('error', (err) => {
          logger.warn(`ImmorTerm AI: git watcher error for ${projectDir}, re-arming: ${err}`);
          try { w.close(); } catch { /* best effort */ }
          this.gitWatchers.delete(projectDir);
          setTimeout(() => { if (!this.gitWatchers.has(projectDir)) arm(); }, 1000);
        });
        this.gitWatchers.set(projectDir, w);
      } catch (e) {
        logger.warn(`ImmorTerm AI: failed to watch git dir for ${projectDir}: ${e}`);
      }
    };

    arm();
  }

  /** Push the currently-active session's branch label to the webview.
   *  The webview's status-bar projectName is a single global variable in the
   *  renderer, so we update it whenever the active tab changes OR the active
   *  session's branch changes. */
  private pushActiveBranchToWebview(): void {
    if (!this.activeWindowId) return;
    let activeSession: { branch?: string } | undefined;
    for (const s of this.sessions.values()) {
      if (s.windowId === this.activeWindowId) { activeSession = s; break; }
    }
    const branch = activeSession?.branch;
    if (branch === this.lastSentBranch) return;
    this.lastSentBranch = branch;
    const displayProjectName = branch
      ? `${this.projectName} ⎇ ${branch}`
      : this.projectName;
    this.view?.webview.postMessage({
      type: 'branch-update',
      projectName: displayProjectName,
      branch,
    });
  }

  private stopAllGitWatchers(): void {
    for (const w of this.gitWatchers.values()) {
      try { w.close(); } catch { /* best effort */ }
    }
    this.gitWatchers.clear();
  }
}

// ── Helper Functions ──

/**
 * Read the current git branch for `projectPath`. Handles worktrees (where
 * `.git` is a file pointing at the real gitdir) and detached HEAD (returns
 * a 7-char SHA prefix). Returns undefined when the dir isn't a git repo.
 */
function detectGitBranch(projectPath: string): string | undefined {
  if (!projectPath) return undefined;
  try {
    let gitDir = path.join(projectPath, '.git');
    const stat = fs.statSync(gitDir, { throwIfNoEntry: false });
    if (!stat) return undefined;
    if (stat.isFile()) {
      const pointer = fs.readFileSync(gitDir, 'utf8').trim();
      const match = pointer.match(/^gitdir:\s*(.+)$/);
      if (!match) return undefined;
      gitDir = path.isAbsolute(match[1]) ? match[1] : path.resolve(projectPath, match[1]);
    }
    const head = fs.readFileSync(path.join(gitDir, 'HEAD'), 'utf8').trim();
    const refMatch = head.match(/^ref:\s*refs\/heads\/(.+)$/);
    if (refMatch) return refMatch[1];
    if (/^[0-9a-f]{40}$/i.test(head)) return head.slice(0, 7);
    return undefined;
  } catch {
    return undefined;
  }
}

function decodeHtmlEntities(s: string): string {
  return s
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&#(\d+);/g, (_, n) => String.fromCharCode(parseInt(n, 10)))
    .replace(/&#x([0-9a-f]+);/gi, (_, h) => String.fromCharCode(parseInt(h, 16)));
}

const DAEMON_INSTALL_DIR = path.join(os.homedir(), '.immorterm', 'bin');
const DAEMON_INSTALL_PATH = path.join(DAEMON_INSTALL_DIR, 'immorterm-ai');
const GITHUB_REPO = 'ImmorTerm/immorterm';

/**
 * Resolve a terminal-detected path string to an absolute filesystem path.
 * - `~` / `~/x` → home
 * - relative → joined with `cwd` (session cwd) or workspace folder fallback
 */
function resolveLinkPath(p: string, cwd?: string): string {
  if (!p) return p;
  if (p === '~') return os.homedir();
  if (p.startsWith('~/')) return path.join(os.homedir(), p.slice(2));
  if (path.isAbsolute(p)) return p;
  const base = cwd || vscode.workspace.workspaceFolders?.[0]?.uri.fsPath || process.cwd();
  return path.resolve(base, p);
}

/**
 * Async variant: first tries the sync resolve (absolute, ~, cwd-relative),
 * then falls back to each workspace folder root, then to a workspace-wide
 * glob search. Lets bare paths like `src/gpu-terminal.ts` resolve to
 * `apps/extension/src/gpu-terminal.ts` when that's the only match.
 */
// Memoized workspace-wide resolution: hover events fire dozens of times in a
// few ms, so without this we'd spam vscode.workspace.findFiles and stall the
// terminal. Keyed by the relative path — basename lookup is workspace-wide
// so `cwd` doesn't change the result.
const workspaceFindCache = new Map<string, Promise<string | null>>();

function findInWorkspaceByBasename(p: string): Promise<string | null> {
  const cached = workspaceFindCache.get(p);
  if (cached) return cached;
  const promise = (async () => {
    const base = path.basename(p);
    const suffixPosix = '/' + p;
    const suffixNative = path.sep + p.split('/').join(path.sep);
    try {
      // undefined exclude → respects files.exclude + search.exclude
      // (node_modules, .git, target/, etc). Passing null here made `findFiles`
      // traverse hundreds of thousands of files and froze the terminal.
      const matches = await vscode.workspace.findFiles(`**/${base}`, undefined, 10);
      const hit = matches.find(u => u.fsPath.endsWith(suffixNative) || u.fsPath.endsWith(suffixPosix));
      if (hit) {
        logger.info(`resolveLinkPathAsync: suffix match for '${p}' → ${hit.fsPath}`);
        return hit.fsPath;
      }
      return null;
    } catch (e: any) {
      logger.warn(`resolveLinkPathAsync: findFiles threw for '${p}': ${e?.message ?? e}`);
      return null;
    }
  })();
  workspaceFindCache.set(p, promise);
  return promise;
}

async function resolveLinkPathAsync(p: string, cwd?: string): Promise<string> {
  const primary = resolveLinkPath(p, cwd);
  try { if (fs.statSync(primary)) return primary; } catch { /* fallthrough */ }
  if (p && !path.isAbsolute(p) && !p.startsWith('~')) {
    for (const folder of vscode.workspace.workspaceFolders ?? []) {
      const candidate = path.join(folder.uri.fsPath, p);
      try { if (fs.statSync(candidate)) return candidate; } catch { /* try next */ }
    }
    const hit = await findInWorkspaceByBasename(p);
    if (hit) return hit;
  }
  return primary;
}

function getDaemonAssetName(): string {
  const platform = process.platform === 'darwin' ? 'macos' : 'linux';
  const arch = process.arch === 'arm64' ? 'aarch64' : 'x86_64';
  return `immorterm-ai-${platform}-${arch}.tar.gz`;
}

async function downloadDaemonBinary(): Promise<string | null> {
  const assetName = getDaemonAssetName();

  return vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: 'ImmorTerm', cancellable: false },
    async (progress) => {
      try {
        progress.report({ message: 'Downloading AI terminal daemon...' });

        // Find latest ai-* release via GitHub API (no gh CLI needed)
        const releasesRaw = await httpsGet(
          `https://api.github.com/repos/${GITHUB_REPO}/releases`,
          { 'User-Agent': 'ImmorTerm-VSCode', 'Accept': 'application/vnd.github+json' },
        );
        const releases = JSON.parse(releasesRaw);
        const aiRelease = releases.find((r: any) => r.tag_name?.startsWith('ai-'));
        if (!aiRelease) {
          logger.warn('ImmorTerm AI: no ai-* release found on GitHub');
          return null;
        }

        const asset = aiRelease.assets?.find((a: any) => a.name === assetName);
        if (!asset) {
          logger.warn(`ImmorTerm AI: asset ${assetName} not found in release ${aiRelease.tag_name}`);
          return null;
        }

        // Ensure install directory exists
        fs.mkdirSync(DAEMON_INSTALL_DIR, { recursive: true });

        const tmpTar = path.join(os.tmpdir(), assetName);

        progress.report({ message: `Downloading ${aiRelease.tag_name}...` });
        await httpsDownload(asset.browser_download_url, tmpTar);

        progress.report({ message: 'Installing...' });
        await new Promise<void>((resolve, reject) => {
          execFile('tar', ['xzf', tmpTar, '-C', DAEMON_INSTALL_DIR], (err) => err ? reject(err) : resolve());
        });
        fs.chmodSync(DAEMON_INSTALL_PATH, 0o755);

        // Cleanup
        try { fs.unlinkSync(tmpTar); } catch { /* ignore */ }

        logger.info(`ImmorTerm AI: daemon installed to ${DAEMON_INSTALL_PATH} (${aiRelease.tag_name})`);
        vscode.window.showInformationMessage(`ImmorTerm AI terminal daemon installed (${aiRelease.tag_name})`);
        return DAEMON_INSTALL_PATH;
      } catch (err: any) {
        logger.error(`ImmorTerm AI: daemon download failed: ${err.message}`);
        vscode.window.showErrorMessage(
          `ImmorTerm AI: failed to download daemon. Install manually: npx immorterm`
        );
        return null;
      }
    },
  );
}

function httpsGet(url: string, headers: Record<string, string>): Promise<string> {
  return new Promise((resolve, reject) => {
    const req = https.get(url, { headers }, (res) => {
      if (res.statusCode === 301 || res.statusCode === 302) {
        httpsGet(res.headers.location!, headers).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        reject(new Error(`HTTP ${res.statusCode} from ${url}`));
        return;
      }
      const chunks: Buffer[] = [];
      res.on('data', (c) => chunks.push(c));
      res.on('end', () => resolve(Buffer.concat(chunks).toString()));
      res.on('error', reject);
    });
    req.on('error', reject);
    req.setTimeout(30_000, () => { req.destroy(); reject(new Error('Timeout')); });
  });
}

function httpsDownload(url: string, dest: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const req = https.get(url, (res) => {
      if (res.statusCode === 301 || res.statusCode === 302) {
        httpsDownload(res.headers.location!, dest).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        reject(new Error(`HTTP ${res.statusCode} downloading ${url}`));
        return;
      }
      const file = fs.createWriteStream(dest);
      res.pipe(file);
      file.on('finish', () => { file.close(); resolve(); });
      file.on('error', reject);
    });
    req.on('error', reject);
    req.setTimeout(120_000, () => { req.destroy(); reject(new Error('Download timeout')); });
  });
}

async function findDaemonBinary(): Promise<string | null> {
  const home = os.homedir();
  const locations = [
    // Primary: installed via `npx immorterm` setup or auto-download
    DAEMON_INSTALL_PATH,
    // Fallback: system-wide install
    '/usr/local/bin/immorterm-ai',
    // Dev: local build
    path.join(home, 'Development', 'immorterm', 'target', 'release', 'immorterm-ai'),
    path.join(home, 'Development', 'immorterm', 'target', 'debug', 'immorterm-ai'),
  ];

  for (const loc of locations) {
    if (fs.existsSync(loc)) return loc;
  }

  // Check PATH
  const which = await new Promise<string | null>((resolve) => {
    execFile('which', ['immorterm-ai'], (err, stdout) => {
      resolve(err || !stdout.trim() ? null : stdout.trim());
    });
  });
  if (which) return which;

  // Not found anywhere — auto-download from GitHub releases
  return downloadDaemonBinary();
}

function spawnDaemon(
  binary: string,
  sessionName: string,
  projectDir?: string,
  windowId?: string,
  displayName?: string,
  claudeSessionId?: string,
  titleLocked?: boolean,
  noAutoResume?: boolean,
): Promise<number | undefined> {
  const shell = process.env.SHELL || '/bin/zsh';
  const args = ['-dmS', sessionName, '-s', shell];

  // Raw .log path must be anchored on windowId (always-safe format: digits-hex),
  // never on sessionName — a user-controllable string that could start with `-`
  // and poison argv / filesystem paths (e.g. the "--help.log" zombie pattern).
  // Structured grid.jsonl/.cast/.ai.jsonl already use windowId-based dirs and are
  // the authoritative scrollback source; this flat .log is kept as legacy safety net.
  if (projectDir) {
    const logsDir = path.join(projectDir, '.immorterm', 'terminals', 'logs');
    try { fs.mkdirSync(logsDir, { recursive: true }); } catch { /* best effort */ }
    const fileStem = windowId && windowId.length > 0 ? windowId : sessionName;
    const logPath = path.join(logsDir, `${fileStem}.log`);
    args.push('-L', '-Logfile', logPath);
  }

  const env: Record<string, string | undefined> = {
    ...process.env,
    IMMORTERM_SESSION: sessionName,
    IMMORTERM_SESSION_TYPE: 'ai',
    TERM: 'xterm-256color',
    // Don't set TERM_PROGRAM=ImmorTerm here — that triggers shell statusline
    // injection which creates a SECOND status bar. GPU renderer draws its own.
  };

  // Pass identity fields so the daemon records them in registry.json.
  // windowId is the immutable unique identifier; displayName is the friendly label.
  // OSC title changes or user renames never affect windowId.
  if (windowId) {
    env.IMMORTERM_WINDOW_ID = windowId;
  }
  if (displayName) {
    env.IMMORTERM_DISPLAY_NAME = displayName;
  }

  // Pass project directory so the daemon records it in registry.json
  if (projectDir) {
    env.SCREEN_PROJECT_DIR = projectDir;
  }
  // Pass Claude session ID so the daemon auto-resumes on restore
  if (claudeSessionId) {
    env.IMMORTERM_CLAUDE_SESSION_ID = claudeSessionId;
  }
  // Block ALL recall tiers when shelve detected user explicitly exited claude.
  // Without this, the daemon's tier-4 fallback (`claude-env/*.env` mtime
  // scan) finds a leftover env file from a prior claude run and resurrects
  // the UUID — auto-resuming a session the user deliberately ended.
  if (noAutoResume) {
    env.IMMORTERM_NO_AUTO_RESUME = '1';
  }
  // Lock title so Claude's OSC title escape sequences don't overwrite custom names
  if (titleLocked) {
    env.IMMORTERM_TITLE_LOCKED = '1';
  }
  // Pass channel setting so shell-init.zsh can enable/disable the claude wrapper
  if (vscode.workspace.getConfiguration('immorterm').get<boolean>('claudeChannelsEnabled') === false) {
    env.IMMORTERM_CHANNELS_ENABLED = 'false';
  }

  const child = spawn(binary, args, {
    cwd: projectDir || process.env.HOME,
    env,
    detached: true,
    stdio: 'ignore',
  });
  const pid = child.pid;
  child.unref();
  return Promise.resolve(pid);
}

async function waitForWsPort(sessionName: string, timeoutMs: number, daemonPid?: number): Promise<number | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const port = findWsPort(sessionName, daemonPid);
    if (port) return port;
    await sleep(100);
  }
  return null;
}

/**
 * Find the WebSocket port for a daemon session.
 * If daemonPid is provided, only match the socket file for that specific PID
 * (avoids stale files from dead daemons with the same session name).
 */
function findWsPort(sessionName: string, daemonPid?: number): number | null {
  try {
    if (!fs.existsSync(SOCKET_DIR)) return null;
    const files = fs.readdirSync(SOCKET_DIR);
    const suffix = `.${sessionName}.ws`;
    let wsFile: string | undefined;
    if (daemonPid) {
      // Exact match: <pid>.<sessionName>.ws
      wsFile = files.find(f => f === `${daemonPid}${suffix}`);
    }
    if (!wsFile) {
      // Fallback: any file ending with .<sessionName>.ws — prefer live PIDs
      const candidates = files.filter(f => f.endsWith(suffix));
      const liveCandidate = candidates.find(f => {
        const pid = parseInt(f.split('.')[0], 10);
        return !isNaN(pid) && isProcessAlive(pid);
      });
      // When waiting for a specific daemon (daemonPid provided), NEVER return
      // stale ports from dead daemons — return null so waitForWsPort() keeps
      // polling until the correct .ws file appears. Without this, stale .ws
      // files from previous dead daemons are returned immediately, causing
      // the webview to connect to a non-listening port → "Disconnected".
      wsFile = liveCandidate || (daemonPid ? undefined : candidates[0]);
    }
    if (!wsFile) return null;
    const content = fs.readFileSync(path.join(SOCKET_DIR, wsFile), 'utf-8').trim();
    const port = parseInt(content, 10);
    return port > 0 ? port : null;
  } catch {
    return null;
  }
}

/**
 * Remove stale .ws port files for a session name where the owning PID is dead.
 * Called before spawning a replacement daemon to prevent findWsPort() from
 * returning a dead port during the polling window.
 */
function cleanupStaleWsFiles(sessionName: string): void {
  try {
    if (!fs.existsSync(SOCKET_DIR)) return;
    const suffix = `.${sessionName}.ws`;
    const files = fs.readdirSync(SOCKET_DIR).filter(f => f.endsWith(suffix));
    for (const file of files) {
      const pid = parseInt(file.split('.')[0], 10);
      if (!isNaN(pid) && !isProcessAlive(pid)) {
        fs.unlinkSync(path.join(SOCKET_DIR, file));
        logger.debug(`ImmorTerm AI: cleaned up stale .ws file: ${file}`);
      }
    }
  } catch {
    // Best effort — non-fatal
  }
}

/**
 * Extract the real daemon PID from the .ws port file for a session.
 * The daemon double-forks, so the PID from Node's spawn() is NOT the daemon PID.
 * The actual daemon PID is encoded in the filename: {daemonPid}.{sessionName}.ws
 */
function findDaemonPidFromWsFile(sessionName: string): number | undefined {
  try {
    if (!fs.existsSync(SOCKET_DIR)) return undefined;
    const suffix = `.${sessionName}.ws`;
    const files = fs.readdirSync(SOCKET_DIR).filter(f => f.endsWith(suffix));
    // Prefer a file whose PID is alive
    const liveFile = files.find(f => {
      const pid = parseInt(f.split('.')[0], 10);
      return !isNaN(pid) && isProcessAlive(pid);
    });
    const file = liveFile || files[0];
    if (file) {
      const pid = parseInt(file.split('.')[0], 10);
      return isNaN(pid) ? undefined : pid;
    }
    return undefined;
  } catch {
    return undefined;
  }
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0); // signal 0 = existence check, no actual signal sent
    return true;
  } catch {
    return false;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let result = '';
  for (let i = 0; i < 32; i++) {
    result += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return result;
}

/**
 * Find the user's VS Code terminal font file and return it as base64.
 * Falls back to null (the WASM side uses embedded JetBrains Mono).
 */
async function findTerminalFontData(): Promise<{ data: string; name: string } | null> {
  try {
    const fontFamily =
      vscode.workspace.getConfiguration('terminal.integrated').get<string>('fontFamily') ||
      vscode.workspace.getConfiguration('editor').get<string>('fontFamily') ||
      '';

    // Extract the first font name from the CSS font-family list
    const firstName = fontFamily.split(',')[0].trim().replace(/['"]/g, '');
    if (!firstName || firstName === 'monospace') {
      // No custom font configured — use platform default
      return findPlatformDefaultFont();
    }

    return findFontByName(firstName);
  } catch (err) {
    logger.warn(`ImmorTerm AI: font discovery failed: ${err}`);
    return null;
  }
}

/** Find a font file by its family name. Returns data + resolved name. */
function findFontByName(fontName: string): { data: string; name: string } | null {
  const home = process.env.HOME || process.env.USERPROFILE || '~';
  const normalized = fontName.replace(/\s+/g, '');
  const extensions = ['.ttc', '.ttf', '.otf'];

  // Platform-specific font search directories
  const searchDirs: string[] = [];
  if (process.platform === 'darwin') {
    searchDirs.push('/System/Library/Fonts', '/Library/Fonts', path.join(home, 'Library', 'Fonts'));
  } else if (process.platform === 'win32') {
    searchDirs.push(path.join(process.env.WINDIR || 'C:\\Windows', 'Fonts'));
    searchDirs.push(path.join(home, 'AppData', 'Local', 'Microsoft', 'Windows', 'Fonts'));
  } else {
    // Linux / FreeBSD
    searchDirs.push('/usr/share/fonts', '/usr/local/share/fonts', path.join(home, '.local', 'share', 'fonts'));
    searchDirs.push('/usr/share/fonts/truetype', '/usr/share/fonts/opentype');
  }

  for (const dir of searchDirs) {
    for (const ext of extensions) {
      const candidates = [
        path.join(dir, `${fontName}${ext}`),          // "Menlo.ttc"
        path.join(dir, `${normalized}${ext}`),         // "JetBrainsMono.ttc"
        path.join(dir, `${normalized}-Regular${ext}`), // "JetBrainsMono-Regular.ttf"
      ];
      for (const p of candidates) {
        if (fs.existsSync(p)) {
          logger.info(`ImmorTerm AI: using terminal font: ${p} (family: ${fontName})`);
          return { data: fs.readFileSync(p).toString('base64'), name: fontName };
        }
      }
    }

    // Recursive search in subdirectories (Linux fonts are often nested)
    if (process.platform !== 'darwin' && fs.existsSync(dir)) {
      const result = findFontInDir(dir, fontName, normalized, extensions);
      if (result) return result;
    }
  }

  logger.info(`ImmorTerm AI: font '${fontName}' not found on disk, using embedded font`);
  return null;
}

/** Recursively search a directory for a font file (max depth 3). */
function findFontInDir(
  dir: string, fontName: string, normalized: string, extensions: string[], depth = 0
): { data: string; name: string } | null {
  if (depth > 3) return null;
  try {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      if (entry.isDirectory()) {
        const result = findFontInDir(path.join(dir, entry.name), fontName, normalized, extensions, depth + 1);
        if (result) return result;
      } else if (entry.isFile()) {
        const lower = entry.name.toLowerCase();
        const nameNorm = normalized.toLowerCase();
        if (extensions.some(ext => lower === `${nameNorm}${ext}` || lower === `${nameNorm}-regular${ext}`)) {
          const p = path.join(dir, entry.name);
          logger.info(`ImmorTerm AI: using terminal font: ${p} (family: ${fontName})`);
          return { data: fs.readFileSync(p).toString('base64'), name: fontName };
        }
      }
    }
  } catch { /* permission denied, etc */ }
  return null;
}

/** Parse VS Code's fontWeight setting into a numeric CSS font-weight (100-900).
 *  Handles keywords ("normal" → 400, "bold" → 700) and numeric strings ("500" → 500).
 */
function parseFontWeight(value: string): number {
  const lower = value.toLowerCase().trim();
  if (lower === 'normal') return 400;
  if (lower === 'bold') return 700;
  const num = parseInt(lower, 10);
  if (!isNaN(num) && num >= 100 && num <= 900) return num;
  return 400; // default
}

/** Try the platform's default monospace terminal font, in preference order. */
function findPlatformDefaultFont(): { data: string; name: string } | null {
  // Each platform has a preferred default font stack — try in order
  const candidates: { name: string; paths: string[] }[] = [];

  if (process.platform === 'darwin') {
    candidates.push(
      { name: 'Menlo', paths: ['/System/Library/Fonts/Menlo.ttc'] },
      { name: 'SF Mono', paths: ['/System/Library/Fonts/SFMono.ttf', '/Library/Fonts/SF-Mono-Regular.otf'] },
      { name: 'Monaco', paths: ['/System/Library/Fonts/Monaco.ttf'] },
    );
  } else if (process.platform === 'win32') {
    const winFonts = path.join(process.env.WINDIR || 'C:\\Windows', 'Fonts');
    candidates.push(
      { name: 'Cascadia Mono', paths: [path.join(winFonts, 'CascadiaMono.ttf'), path.join(winFonts, 'CascadiaMono-Regular.ttf')] },
      { name: 'Consolas', paths: [path.join(winFonts, 'consola.ttf')] },
      { name: 'Lucida Console', paths: [path.join(winFonts, 'lucon.ttf')] },
    );
  } else {
    // Linux / FreeBSD — check common monospace font locations
    candidates.push(
      { name: 'DejaVu Sans Mono', paths: ['/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf'] },
      { name: 'Liberation Mono', paths: ['/usr/share/fonts/truetype/liberation/LiberationMono-Regular.ttf'] },
      { name: 'Ubuntu Mono', paths: ['/usr/share/fonts/truetype/ubuntu/UbuntuMono-R.ttf'] },
      { name: 'Noto Sans Mono', paths: ['/usr/share/fonts/truetype/noto/NotoSansMono-Regular.ttf'] },
    );
  }

  for (const { name, paths } of candidates) {
    for (const p of paths) {
      if (fs.existsSync(p)) {
        logger.info(`ImmorTerm AI: using platform default font: ${p} (${name})`);
        return { data: fs.readFileSync(p).toString('base64'), name };
      }
    }
  }

  logger.info('ImmorTerm AI: no platform default font found, using embedded JetBrains Mono');
  return null;
}
