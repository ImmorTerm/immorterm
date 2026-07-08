import * as vscode from 'vscode';
import { WorkspaceStorage } from '../storage/workspace-state';
import { screenCommands } from '../utils/screen-commands';
import { logger } from '../utils/logger';
import { isStatusBarEnabled } from '../utils/settings';
import { exec } from 'child_process';
import { promisify } from 'util';
import {
  isMemoryEnabled,
  getOpenMemoryState,
  refreshOpenMemoryState,
} from '../services/memory';
import { getMemoryPort } from '../services/memory/native-memory-manager';
import {
  isGatewayEnabled,
  getMCPGatewayState,
} from '../services/mcp-gateway';

const execAsync = promisify(exec);

/**
 * Status bar item for ImmorTerm
 * Shows terminal count and Screen availability status
 */
export class StatusBar {
  private statusBarItem: vscode.StatusBarItem;
  private storage: WorkspaceStorage | null = null;
  private screenAvailable: boolean = false;
  private disposed: boolean = false;
  private immortermVersion: string = '';

  constructor() {
    // Create status bar item aligned left with priority 100
    this.statusBarItem = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      100
    );

    // Set command to show status when clicked
    this.statusBarItem.command = 'immorterm.showStatus';

    // Set initial tooltip
    this.statusBarItem.tooltip = 'ImmorTerm: Click for status';

    logger.debug('StatusBar created');
  }

  /**
   * Initializes the status bar with storage and Screen availability
   * @param storage The workspace storage instance
   * @param screenAvailable Whether GNU Screen is available
   */
  async initialize(storage: WorkspaceStorage, screenAvailable: boolean): Promise<void> {
    this.storage = storage;
    this.screenAvailable = screenAvailable;

    // Fetch ImmorTerm version
    await this.fetchVersion();

    await this.update();
    this.show();

    logger.debug('StatusBar initialized, Screen available:', screenAvailable, 'version:', this.immortermVersion);
  }

  /**
   * Fetches the ImmorTerm extension version
   * Note: We use extension version since screen's -v flag doesn't work non-interactively
   */
  private async fetchVersion(): Promise<void> {
    try {
      const ext = vscode.extensions.getExtension('immorterm.immorterm-extension');
      if (ext) {
        this.immortermVersion = `ImmorTerm ${ext.packageJSON.version}`;
        logger.info('ImmorTerm extension version:', this.immortermVersion);
      } else {
        this.immortermVersion = 'ImmorTerm';
        logger.warn('Could not find ImmorTerm extension info');
      }
    } catch (error) {
      this.immortermVersion = 'ImmorTerm';
      logger.warn('Failed to get ImmorTerm version:', error);
    }
  }

  /**
   * Updates the status bar text and icon
   */
  async update(): Promise<void> {
    if (this.disposed) {
      return;
    }

    // Check if status bar is enabled via settings utility
    if (!isStatusBarEnabled()) {
      this.hide();
      return;
    }

    // Build status text
    let icon: string;
    let text: string;
    let tooltip: string;

    if (this.storage) {
      // Show terminal count
      const terminalCount = this.storage.getTerminalCount();
      const projectName = this.storage.getProjectName();

      icon = '$(terminal)';
      text = `ImmorTerm: ${terminalCount}`;
      tooltip = `${this.immortermVersion}\nProject: ${projectName}\n${terminalCount} terminal${terminalCount !== 1 ? 's' : ''} registered`;

      // Get active Screen session count for tooltip
      try {
        const sessions = await screenCommands.listProjectSessions(projectName);
        tooltip += `\n${sessions.length} session${sessions.length !== 1 ? 's' : ''} active`;
      } catch {
        // Ignore errors in tooltip generation
      }

      // Add memory service status if enabled
      if (isMemoryEnabled()) {
        const serviceState = getOpenMemoryState();

        // Add brain icon to status bar text when memory is active
        if (serviceState.apiHealthy && serviceState.mcpHealthy) {
          text += ' 🧠'; // Brain icon for OpenMemory fully active
        } else if (serviceState.apiHealthy && !serviceState.mcpHealthy) {
          text += ' 🧠$(warning)'; // API ok but MCP degraded
        } else if (serviceState.activeClaudeSessions.size > 0) {
          text += ' 🧠$(warning)'; // Memory enabled but services not ready
        }

        // Add memory status to tooltip
        tooltip += '\n───────────────';
        tooltip += '\nOpenMemory:';
        tooltip += `\n  API: ${serviceState.apiHealthy ? '✓ Running' : serviceState.stackRunning ? '⏳ Starting...' : '✗ Not running'}`;
        tooltip += `\n  MCP: ${serviceState.mcpHealthy ? '✓ Connected' : serviceState.apiHealthy ? '✗ Degraded (auto-recovering)' : '✗ Not available'}`;
        if (serviceState.activeClaudeSessions.size > 0) {
          tooltip += `\n  Claude sessions: ${serviceState.activeClaudeSessions.size}`;
        }
        if (serviceState.apiHealthy) {
          tooltip += `\n  Web UI: http://localhost:${getMemoryPort()}`;
        }
      }

      // Add MCP Gateway status if enabled
      if (isGatewayEnabled()) {
        const gwState = getMCPGatewayState();

        // Add wireless icon to status bar text when gateway is active
        if (gwState.healthy) {
          text += ' 📡'; // Antenna icon for MCP Gateway active
        } else if (gwState.running) {
          text += ' 📡$(warning)'; // Gateway running but unhealthy
        }

        // Add gateway status to tooltip
        tooltip += '\n───────────────';
        tooltip += '\nMCP Gateway:';
        tooltip += `\n  Status: ${gwState.healthy ? '✓ Running' : gwState.running ? '⏳ Starting...' : '✗ Not running'}`;
        if (gwState.healthy) {
          tooltip += `\n  Servers: ${gwState.serverCount ?? 0}, Active: ${gwState.activeChildren ?? 0}`;
          if (gwState.memoryMB) {
            tooltip += `\n  Memory: ${gwState.memoryMB} MB`;
          }
        }
        if (gwState.lastError) {
          tooltip += `\n  Error: ${gwState.lastError}`;
        }
      }

      // Add hook health status
      try {
        const hookStatus = await this.getHookStatus();
        if (hookStatus) {
          tooltip += '\n───────────────';
          tooltip += `\n${hookStatus}`;
        }
      } catch {
        // Ignore hook health errors in tooltip
      }

      tooltip += '\nClick to show status';
    } else {
      // Not initialized yet
      icon = '$(terminal)';
      text = 'ImmorTerm';
      tooltip = 'ImmorTerm: Initializing...';
    }

    this.statusBarItem.text = `${icon} ${text}`;
    this.statusBarItem.tooltip = tooltip;
    this.statusBarItem.backgroundColor = undefined;

    logger.debug('StatusBar updated:', text);
  }

  /**
   * Shows the status bar item
   */
  show(): void {
    if (this.disposed) {
      return;
    }

    // Only show if enabled via settings utility
    if (isStatusBarEnabled()) {
      this.statusBarItem.show();
    }
  }

  /**
   * Hides the status bar item
   */
  hide(): void {
    this.statusBarItem.hide();
  }

  /**
   * Sets the Screen availability status
   * @param available Whether Screen is available
   */
  async setScreenAvailable(available: boolean): Promise<void> {
    this.screenAvailable = available;
    await this.update();
  }

  /**
   * Sets the storage instance
   * @param storage The workspace storage
   */
  setStorage(storage: WorkspaceStorage): void {
    this.storage = storage;
  }

  /**
   * Gets whether Screen is available
   */
  isScreenAvailable(): boolean {
    return this.screenAvailable;
  }

  /**
   * Gets the status bar item (for testing)
   */
  getStatusBarItem(): vscode.StatusBarItem {
    return this.statusBarItem;
  }

  /**
   * Checks hook health and returns a tooltip string, or null if nothing notable.
   */
  private async getHookStatus(): Promise<string | null> {
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (!workspaceFolders?.length) return null;

    const projectRoot = workspaceFolders[0].uri.fsPath;
    const fs = await import('node:fs');
    const path = await import('node:path');

    const errorsDir = path.join(projectRoot, '.immorterm', 'terminals', 'hooks', 'errors');
    let errorCount = 0;
    try {
      const files = fs.readdirSync(errorsDir).filter((f: string) => f.endsWith('.log'));
      for (const f of files) {
        try {
          const stat = fs.statSync(path.join(errorsDir, f));
          if (stat.size > 0) errorCount++;
        } catch { /* skip */ }
      }
    } catch { /* no errors dir */ }

    const logsDir = path.join(projectRoot, '.immorterm', 'terminals', 'hooks', 'logs');
    let recentActivity = false;
    try {
      const logFiles = fs.readdirSync(logsDir).filter((f: string) => f.endsWith('.log'));
      const now = Date.now();
      for (const f of logFiles) {
        try {
          const stat = fs.statSync(path.join(logsDir, f));
          if (now - stat.mtimeMs < 3600_000) { recentActivity = true; break; }
        } catch { /* skip */ }
      }
    } catch { /* no logs dir */ }

    const parts: string[] = [];
    if (errorCount > 0) {
      parts.push(`Hooks: ${errorCount} error(s)`);
    }
    if (!recentActivity) {
      parts.push('Hooks: stale');
    }

    // Check digest daemon (requires project config for projectId)
    try {
      const configPath = path.join(projectRoot, '.immorterm', 'config.json');
      const config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
      const projectId = config.projectId;
      if (projectId && isMemoryEnabled()) {
        const os = await import('node:os');
        const pidFile = path.join(os.homedir(), '.immorterm', `digest-daemon-${projectId}.pid`);
        let daemonAlive = false;
        try {
          const pid = parseInt(fs.readFileSync(pidFile, 'utf-8').trim(), 10);
          if (!isNaN(pid)) {
            process.kill(pid, 0);
            daemonAlive = true;
          }
        } catch { /* not running or no PID file */ }
        if (!daemonAlive) {
          parts.push('Digest daemon stopped');
        }
      }
    } catch { /* no config */ }

    return parts.length > 0 ? parts.join(' | ') : null;
  }

  /**
   * Disposes of the status bar item
   */
  dispose(): void {
    this.disposed = true;
    this.statusBarItem.dispose();
    logger.debug('StatusBar disposed');
  }
}

export default StatusBar;
