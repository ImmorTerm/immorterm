/**
 * ImmorTerm Team View — multi-pane GPU terminal for Claude Code Agent Teams.
 *
 * Watches ~/.claude/teams/ for active teams, parses team state (config, tasks,
 * messages), and renders multiple WASM terminal panes in a VS Code webview.
 *
 * Architecture:
 *   Extension watches teams dir → detects active team → parses state
 *   → sends to webview → webview creates N WasmTerminal instances
 *   → each connects via WebSocket to member's daemon session
 */

import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';
import { logger } from './utils/logger';

const TEAMS_DIR = path.join(process.env.HOME || '~', '.claude', 'teams');
const TASKS_DIR = path.join(process.env.HOME || '~', '.claude', 'tasks');
const SOCKET_DIR = path.join(process.env.HOME || '~', '.immorterm', 'sockets');
const WASM_RESOURCE_DIR = 'resources/wasm';

export const TEAM_VIEW_ID = 'immorterm.teamView';

/**
 * Provides the ImmorTerm Team View as a WebviewView in the bottom panel.
 *
 * Detects active Claude Code agent teams, renders multi-pane GPU terminals
 * for each teammate, and shows a dashboard with task/message state.
 */
export class TeamViewProvider implements vscode.WebviewViewProvider {
  private view?: vscode.WebviewView;
  private context: vscode.ExtensionContext;
  private teamWatcher?: vscode.FileSystemWatcher;
  private taskWatcher?: vscode.FileSystemWatcher;
  private activeTeamName: string | null = null;
  private wasmInitSent = false;
  private disposables: vscode.Disposable[] = [];
  private refreshTimer?: NodeJS.Timeout;

  constructor(context: vscode.ExtensionContext) {
    this.context = context;
  }

  resolveWebviewView(
    webviewView: vscode.WebviewView,
    _context: vscode.WebviewViewResolveContext,
    _token: vscode.CancellationToken,
  ): void {
    this.view = webviewView;
    logger.info('TeamView: WebviewView resolved');

    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.file(path.join(this.context.extensionPath, 'resources')),
      ],
    };

    webviewView.webview.onDidReceiveMessage(
      (msg) => this.handleWebviewMessage(msg),
      null,
      this.disposables,
    );

    webviewView.onDidDispose(() => {
      this.view = undefined;
      this.wasmInitSent = false;
      this.stopWatching();
      logger.info('TeamView: WebviewView disposed');
    });

    this.setWebviewHtml(webviewView.webview);
    this.startWatching();
  }

  /** Start watching for team changes. */
  private startWatching() {
    // Ensure dirs exist
    fs.mkdirSync(TEAMS_DIR, { recursive: true });
    fs.mkdirSync(TASKS_DIR, { recursive: true });

    // Watch teams directory
    const teamsPattern = new vscode.RelativePattern(
      vscode.Uri.file(TEAMS_DIR), '**/*.json'
    );
    this.teamWatcher = vscode.workspace.createFileSystemWatcher(teamsPattern);
    this.teamWatcher.onDidChange(() => this.debouncedRefresh());
    this.teamWatcher.onDidCreate(() => this.debouncedRefresh());
    this.teamWatcher.onDidDelete(() => this.debouncedRefresh());

    // Watch tasks directory
    const tasksPattern = new vscode.RelativePattern(
      vscode.Uri.file(TASKS_DIR), '**/*.json'
    );
    this.taskWatcher = vscode.workspace.createFileSystemWatcher(tasksPattern);
    this.taskWatcher.onDidChange(() => this.debouncedRefresh());
    this.taskWatcher.onDidCreate(() => this.debouncedRefresh());

    // Initial scan
    this.refreshTeamState();
  }

  private stopWatching() {
    this.teamWatcher?.dispose();
    this.taskWatcher?.dispose();
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
  }

  /** Debounce: coalesce rapid file changes into one refresh. */
  private debouncedRefresh() {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => this.refreshTeamState(), 300);
  }

  /** Scan for active teams and send state to webview. */
  private refreshTeamState() {
    if (!this.view) return;

    try {
      const teams = this.discoverTeams();

      if (teams.length === 0) {
        this.activeTeamName = null;
        this.view.webview.postMessage({ type: 'no-team' });
        return;
      }

      // Use the most recently created team (or first if no timestamps)
      const teamName = this.activeTeamName && teams.includes(this.activeTeamName)
        ? this.activeTeamName
        : teams[teams.length - 1];

      this.activeTeamName = teamName;

      const state = this.loadTeamState(teamName);
      if (!state) {
        this.view.webview.postMessage({ type: 'no-team' });
        return;
      }

      // Send WASM URIs once
      this.sendWasmInit();

      // Send team state
      this.view.webview.postMessage({ type: 'team-state', state });

      // Try to connect members to their daemon sessions
      this.connectMembers(state);
    } catch (err) {
      logger.error(`TeamView: refresh failed: ${err}`);
    }
  }

  /** Discover team names from ~/.claude/teams/. */
  private discoverTeams(): string[] {
    try {
      if (!fs.existsSync(TEAMS_DIR)) return [];
      return fs.readdirSync(TEAMS_DIR, { withFileTypes: true })
        .filter(d => d.isDirectory())
        .filter(d => fs.existsSync(path.join(TEAMS_DIR, d.name, 'config.json')))
        .map(d => d.name);
    } catch {
      return [];
    }
  }

  /** Load full team state from disk. */
  private loadTeamState(teamName: string): TeamState | null {
    try {
      // Parse config
      const configPath = path.join(TEAMS_DIR, teamName, 'config.json');
      const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));

      // Parse tasks
      const tasksDir = path.join(TASKS_DIR, teamName);
      const tasks: TeamTask[] = [];
      if (fs.existsSync(tasksDir)) {
        for (const file of fs.readdirSync(tasksDir)) {
          if (!file.endsWith('.json')) continue;
          try {
            const task = JSON.parse(fs.readFileSync(path.join(tasksDir, file), 'utf-8'));
            tasks.push(task);
          } catch { /* skip bad task files */ }
        }
      }
      tasks.sort((a, b) => {
        const aNum = parseInt(a.id, 10);
        const bNum = parseInt(b.id, 10);
        if (!isNaN(aNum) && !isNaN(bNum)) return aNum - bNum;
        return a.id.localeCompare(b.id);
      });

      // Parse inboxes
      const inboxesDir = path.join(TEAMS_DIR, teamName, 'inboxes');
      const inboxes: Record<string, TeamMessage[]> = {};
      if (fs.existsSync(inboxesDir)) {
        for (const file of fs.readdirSync(inboxesDir)) {
          if (!file.endsWith('.json')) continue;
          try {
            const agentName = path.basename(file, '.json');
            const messages = JSON.parse(fs.readFileSync(path.join(inboxesDir, file), 'utf-8'));
            inboxes[agentName] = messages;
          } catch { /* skip */ }
        }
      }

      // Derive member statuses
      const memberStatus: Record<string, string> = {};
      for (const member of config.members || []) {
        const name = member.name;
        const hasActive = tasks.some(
          (t: TeamTask) => t.status === 'in_progress' && t.owner === name
        );
        if (hasActive) { memberStatus[name] = 'active'; continue; }

        const owned = tasks.filter((t: TeamTask) => t.owner === name);
        const allDone = owned.length > 0 && owned.every((t: TeamTask) => t.status === 'completed');
        if (allDone) { memberStatus[name] = 'done'; continue; }

        // Check for idle notification
        const leadInbox = inboxes['team-lead'] || [];
        const hasIdle = leadInbox.slice(-5).some(
          (m: TeamMessage) => m.from === name && m.text?.includes('"type":"idle_notification"')
        );
        if (hasIdle) { memberStatus[name] = 'idle'; continue; }

        memberStatus[name] = 'unknown';
      }

      return { config, tasks, inboxes, member_status: memberStatus };
    } catch (err) {
      logger.warn(`TeamView: failed to load team '${teamName}': ${err}`);
      return null;
    }
  }

  /** Try to match team members to daemon sessions via WebSocket port files. */
  private connectMembers(state: TeamState) {
    if (!this.view) return;

    // For each non-lead member, look for a daemon session
    for (const member of state.config.members) {
      if (member.agentType === 'team-lead') continue;

      const wsPort = this.findMemberWsPort(member.name, state.config.name);
      if (wsPort) {
        logger.info(`TeamView: connecting ${member.name} to ws port ${wsPort}`);
        this.view.webview.postMessage({
          type: 'connect-member',
          memberName: member.name,
          wsPort,
        });
      }
    }
  }

  /** Find WebSocket port for a team member's daemon session. */
  private findMemberWsPort(memberName: string, _teamName: string): number | null {
    try {
      if (!fs.existsSync(SOCKET_DIR)) return null;
      const files = fs.readdirSync(SOCKET_DIR);

      // Look for a .ws file matching the member name
      // Convention: the daemon session name contains the member name
      const wsFile = files.find(f =>
        f.endsWith('.ws') && f.includes(memberName)
      );
      if (!wsFile) return null;

      const content = fs.readFileSync(path.join(SOCKET_DIR, wsFile), 'utf-8').trim();
      const port = parseInt(content, 10);
      return port > 0 ? port : null;
    } catch {
      return null;
    }
  }

  /** Send WASM init URIs to webview (once). */
  private sendWasmInit() {
    if (!this.view || this.wasmInitSent) return;

    const webview = this.view.webview;
    const wasmJsUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.context.extensionPath, WASM_RESOURCE_DIR, 'immorterm_wasm.js')),
    );
    const wasmBgUri = webview.asWebviewUri(
      vscode.Uri.file(path.join(this.context.extensionPath, WASM_RESOURCE_DIR, 'immorterm_wasm_bg.wasm')),
    );

    webview.postMessage({
      type: 'wasm-init',
      wasmJsUri: wasmJsUri.toString(),
      wasmBgUri: wasmBgUri.toString(),
    });
    this.wasmInitSent = true;
  }

  /** Set the webview HTML with proper CSP. */
  private setWebviewHtml(webview: vscode.Webview) {
    const htmlPath = path.join(this.context.extensionPath, 'resources', 'team-terminal.html');
    let html = fs.readFileSync(htmlPath, 'utf-8');

    const nonce = getNonce();
    const csp = [
      `default-src 'none'`,
      `script-src 'nonce-${nonce}' 'unsafe-eval' 'wasm-unsafe-eval'`,
      `style-src 'unsafe-inline'`,
      `connect-src ws://127.0.0.1:*`,
      `img-src ${webview.cspSource}`,
    ].join('; ');

    html = html.replace(
      '<script type="module">',
      `<meta http-equiv="Content-Security-Policy" content="${csp}">\n  <script type="module" nonce="${nonce}">`,
    );

    webview.html = html;
  }

  private handleWebviewMessage(msg: { type: string; [key: string]: unknown }) {
    switch (msg.type) {
      case 'loaded':
        logger.info('TeamView: webview loaded');
        this.refreshTeamState();
        break;

      case 'send-team-message':
        this.sendTeamMessage(
          msg.teamName as string,
          msg.recipient as string,
          msg.content as string,
        );
        break;
    }
  }

  /** Write a message to a teammate's inbox file. */
  private sendTeamMessage(teamName: string, recipient: string, content: string) {
    try {
      const inboxPath = path.join(TEAMS_DIR, teamName, 'inboxes', `${recipient}.json`);
      const inboxDir = path.dirname(inboxPath);
      fs.mkdirSync(inboxDir, { recursive: true });

      let messages: unknown[] = [];
      if (fs.existsSync(inboxPath)) {
        try {
          messages = JSON.parse(fs.readFileSync(inboxPath, 'utf-8'));
        } catch { /* start fresh */ }
      }

      messages.push({
        from: 'immorterm',
        text: content,
        summary: content.length > 60 ? content.slice(0, 60) : content,
        timestamp: new Date().toISOString(),
        color: 'purple',
        read: false,
      });

      fs.writeFileSync(inboxPath, JSON.stringify(messages, null, 2));
      logger.info(`TeamView: sent message to ${recipient} in ${teamName}`);
    } catch (err) {
      logger.error(`TeamView: failed to send message: ${err}`);
    }
  }

  /** Manually set the active team. */
  setActiveTeam(teamName: string) {
    this.activeTeamName = teamName;
    this.refreshTeamState();
  }

  dispose() {
    this.stopWatching();
    this.disposables.forEach(d => d.dispose());
    this.disposables = [];
  }
}

// ── Types ──

interface TeamState {
  config: {
    name: string;
    description: string;
    members: TeamMember[];
  };
  tasks: TeamTask[];
  inboxes: Record<string, TeamMessage[]>;
  member_status: Record<string, string>;
}

interface TeamMember {
  agentId: string;
  name: string;
  agentType: string;
  model: string;
  color?: string;
}

interface TeamTask {
  id: string;
  subject: string;
  description: string;
  activeForm?: string;
  owner?: string;
  status: string;
  blocks: string[];
  blockedBy: string[];
}

interface TeamMessage {
  from: string;
  text: string;
  summary?: string;
  timestamp: string;
  color?: string;
  read: boolean;
}

function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let result = '';
  for (let i = 0; i < 32; i++) {
    result += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return result;
}
