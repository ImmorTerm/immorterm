import * as vscode from 'vscode';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { logger } from './utils/logger';
import { screenCommands } from './utils/screen-commands';
import { extractResources, getTerminalsDir, getLogsDir } from './utils/resource-extractor';
import { getProjectName } from './utils/process';
import { WorkspaceStorage } from './storage/workspace-state';
import { TerminalManager } from './terminal/manager';
import { setScreenAvailable } from './terminal/screen-integration';
import { StatusBar } from './ui/status-bar';
import { notifications } from './ui/notifications';
import { saveDirtyWorkspaceSettings } from './utils/settings';
import {
  getProjectDir,
  getRestoreJsonPath,
  getTerminalsDir as getTerminalsDirConfig,
  ensureGlobalConfig,
  ensureProjectConfig,
  readProjectConfig,
  setEnabledState,
  setServiceEnabled,
  setTheme as setConfigTheme,
  getAppearance,
  updateAppearance,
} from './utils/immorterm-config';
import type { AppearanceConfig } from './utils/immorterm-config';

// Memory services (native Rust binary)
import {
  isMemoryEnabled,
  setMemoryWorkspacePath,
  getStableProjectId,
  installMemoryHooks,
  areHooksInstalled,
  initOpenMemoryManager,
  startOpenMemory,
  configureOpenMemoryMCP,
  configureTerminalMCP,
  removeTerminalMCP,
  migrateFromGlobalConfig,
} from './services/memory';

// MCP Gateway
import { initGatewayManager } from './services/mcp-gateway';

// Dashboard API server (immorterm serve)
import { initServeManager, startServe } from './services/serve-manager';

/**
 * Result of workspace initialization
 */
export interface InitializationResult {
  /** Whether Screen is available */
  screenAvailable: boolean;
  /** The terminal manager instance */
  terminalManager: TerminalManager;
  /** The workspace storage instance */
  storage: WorkspaceStorage;
  /** The status bar instance */
  statusBar: StatusBar;
  /** Path to the terminals directory (.immorterm/terminals/) */
  terminalsDir: string;
  /** Path to the logs directory (.immorterm/terminals/logs/) */
  logsDir: string;
}

/**
 * Migrate data from old .vscode/ layout to new .immorterm/ layout.
 * Safe to call multiple times — idempotent per operation.
 *
 * Handles partial migrations: if .immorterm/terminals/ already exists
 * (e.g., created by a previous extension version before migration logic
 * existed), we still move any remaining data and clean up .vscode/terminals/.
 */
function migrateFromVSCodeLayout(workspacePath: string): void {
  const oldTerminalsDir = path.join(workspacePath, '.vscode', 'terminals');
  const newTerminalsDir = getTerminalsDirConfig(workspacePath);
  const oldJson = path.join(workspacePath, '.vscode', 'restore-terminals.json');
  const newJson = getRestoreJsonPath(workspacePath);

  // Nothing to migrate if old layout doesn't exist
  if (!fs.existsSync(oldTerminalsDir) && !fs.existsSync(oldJson)) {
    return;
  }

  logger.info('[migration] Checking .vscode/ → .immorterm/ migration...');

  try {
    // Ensure .immorterm/terminals/ exists
    if (!fs.existsSync(newTerminalsDir)) {
      fs.mkdirSync(newTerminalsDir, { recursive: true });
    }

    // Move data directories (logs, renames, pending) — skip if destination already exists
    for (const subdir of ['logs', 'renames', 'pending']) {
      const src = path.join(oldTerminalsDir, subdir);
      const dst = path.join(newTerminalsDir, subdir);
      if (fs.existsSync(src) && !fs.existsSync(dst)) {
        fs.renameSync(src, dst);
        logger.info(`[migration] Moved ${subdir}/`);
      }
    }

    // Move restore-terminals.json
    if (fs.existsSync(oldJson) && !fs.existsSync(newJson)) {
      fs.renameSync(oldJson, newJson);
      logger.info('[migration] Moved restore-terminals.json');
    }

    // Rewrite stale .vscode/terminals/screen-auto paths inside restore-terminals.json
    if (fs.existsSync(newJson)) {
      try {
        const raw = fs.readFileSync(newJson, 'utf-8');
        if (raw.includes('.vscode/terminals/screen-auto')) {
          const updated = raw.replace(
            /\.vscode\/terminals\/screen-auto/g,
            '$HOME/.immorterm/scripts/screen-auto'
          );
          fs.writeFileSync(newJson, updated, 'utf-8');
          logger.info('[migration] Rewrote screen-auto paths in restore-terminals.json');
        }
      } catch (err) {
        logger.warn('[migration] Failed to rewrite restore-terminals.json paths:', err);
      }
    }

    // Clean up old .vscode/terminals/ — all scripts moved to ~/.immorterm/scripts/
    if (fs.existsSync(oldTerminalsDir)) {
      fs.rmSync(oldTerminalsDir, { recursive: true, force: true });
      logger.info('[migration] Removed old .vscode/terminals/');
    }

    logger.info('[migration] Directory migration complete');
  } catch (error) {
    logger.warn('[migration] Migration failed (non-fatal):', error);
    // Non-fatal — extension will create new dirs via extractResources
  }
}

/**
 * Migrate GPU appearance settings from VS Code workspace settings to ~/.immorterm/config.json.
 * One-time: checks if any of the 4 GPU appearance keys have non-default values in VS Code settings,
 * writes them to config.json under `appearance`, then clears them from VS Code settings.
 */
async function migrateAppearanceSettings(): Promise<void> {
  try {
    const config = vscode.workspace.getConfiguration('immorterm');

    // The 4 GPU appearance keys that moved to config.json
    const keyMap: Array<{ vsKey: string; configKey: keyof AppearanceConfig; defaultVal: unknown }> = [
      { vsKey: 'borderEnabled', configKey: 'borderEnabled', defaultVal: true },
      { vsKey: 'borderOpacity', configKey: 'borderOpacity', defaultVal: 1.0 },
      { vsKey: 'statusBarAnimations', configKey: 'statusBarAnimations', defaultVal: true },
      { vsKey: 'statusBarMode', configKey: 'statusBarMode', defaultVal: 'always' },
    ];

    const partial: Partial<AppearanceConfig> = {};
    const keysToRemove: string[] = [];

    for (const { vsKey, configKey, defaultVal } of keyMap) {
      const inspection = config.inspect(vsKey);
      // Check workspace or global scope for a user-set value
      const userValue = inspection?.workspaceValue ?? inspection?.globalValue;
      if (userValue !== undefined && userValue !== defaultVal) {
        (partial as Record<string, unknown>)[configKey] = userValue;
        keysToRemove.push(vsKey);
      } else if (userValue !== undefined) {
        // Default value but explicitly set — still clean it up
        keysToRemove.push(vsKey);
      }
    }

    if (keysToRemove.length === 0) return;

    // Write non-default values to config.json
    if (Object.keys(partial).length > 0) {
      updateAppearance(partial);
    }

    // Clear from VS Code settings
    const { saveDirtyWorkspaceSettings } = await import('./utils/settings');
    await saveDirtyWorkspaceSettings();
    for (const key of keysToRemove) {
      await config.update(key, undefined, vscode.ConfigurationTarget.Workspace);
      await config.update(key, undefined, vscode.ConfigurationTarget.Global);
    }

    logger.info(`[migration] Migrated ${keysToRemove.length} appearance settings to config.json`);
  } catch (err) {
    logger.warn('[migration] Appearance settings migration failed (non-fatal):', err);
  }
}

/**
 * Detects if Immorterm is installed and available in PATH
 * @returns true if immorterm is installed, false otherwise
 */
/**
 * Detect screen availability — fully synchronous.
 * Uses execFileSync('which') to avoid yielding the event loop.
 * In high-RSS workspaces, every await gives other extensions CPU time
 * to allocate memory, pushing past the V8 heap limit.
 */
export function detectScreen(): boolean {
  const isInstalled = screenCommands.isScreenInstalledSync();

  if (isInstalled) {
    const screenPath = screenCommands.getScreenPathSync();
    logger.info('Immorterm detected:', screenPath ?? 'unknown path');
  } else {
    logger.warn('Immorterm not found in PATH');
  }

  return isInstalled;
}

/**
 * Start memory services with plug-and-play experience.
 * Automatically handles:
 * - Native memory binary detection and startup
 * - MCP configuration
 * - Hook installation
 *
 * Only shows errors when automatic fixes fail.
 *
 * @param workspacePath Path to the workspace folder
 * @returns true if memory services are ready
 */
async function startMemoryServicesPlugAndPlay(workspacePath: string): Promise<boolean> {
  logger.info('Starting memory services (plug-and-play)...');

  // Get project ID for isolation
  const projectId = getStableProjectId(workspacePath);
  logger.info('Memory project ID:', projectId);

  // Start native memory service
  const state = await startOpenMemory();

  if (state.apiHealthy) {
    // Success! Configure MCP and install hooks
    logger.info('OpenMemory started successfully');

    // Configure MCP with Streamable HTTP transport (per-project isolation via .mcp.json)
    const mcpConfigured = await configureOpenMemoryMCP(workspacePath, projectId);
    if (mcpConfigured) {
      logger.info('MCP configured for project:', projectId);
    } else {
      logger.warn('Failed to configure MCP');
    }

    // Clean up legacy global MCP config from ~/.claude.json (one-time migration)
    migrateFromGlobalConfig();

    // Install/update hooks — always run to pick up new hooks added in extension updates.
    // Generators are idempotent so rewriting is safe.
    installMemoryHooks(workspacePath, projectId);
    logger.info('Memory hooks installed/updated');

    return true;
  }

  // Failed - show actionable error
  logger.warn('Failed to start OpenMemory:', state.lastError);

  vscode.window.showErrorMessage(
    `ImmorTerm Memory: ${state.lastError || 'Failed to start'}`,
    'Run Doctor',
    'Retry',
    'Dismiss'
  ).then(async (selection) => {
    if (selection === 'Run Doctor') {
      vscode.commands.executeCommand('immorterm.doctor');
    } else if (selection === 'Retry') {
      await startMemoryServicesPlugAndPlay(workspacePath);
    }
  });

  return false;
}

/**
 * Initializes the workspace for ImmorTerm
 * - Runs migration from .vscode/ layout if needed
 * - Ensures global and project configs exist
 * - Extracts bundled resources (scripts, templates)
 * - Creates WorkspaceStorage
 * - Creates TerminalManager
 * - Creates StatusBar
 * - Shows warning if Screen is not installed
 *
 * @param context The extension context
 * @returns InitializationResult with all initialized components
 * @throws Error if no workspace folder is open
 */
export async function initializeWorkspace(
  context: vscode.ExtensionContext
): Promise<InitializationResult> {
  logger.info('Initializing ImmorTerm...');

  // Get workspace folder
  const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
  if (!workspaceFolder) {
    logger.warn('No workspace folder found - ImmorTerm requires a workspace');
    throw new Error('ImmorTerm requires a workspace folder to be open');
  }

  const workspacePath = workspaceFolder.uri.fsPath;

  // Crash-proof diagnostic logging: writes directly to disk at each step.
  // If Extension Host OOMs, we'll see exactly which step was last completed.
  const diagPath = path.join(workspacePath, '.immorterm', 'diagnostic.log');
  const diag = (step: string) => {
    try {
      const rss = Math.round(process.memoryUsage().rss / 1024 / 1024);
      fs.appendFileSync(diagPath, `[${new Date().toISOString()}] INIT_STEP: ${step} (RSS=${rss}MB)\n`);
    } catch { /* ignore if .immorterm/ doesn't exist yet */ }
  };
  diag('start');

  // Ensure global config exists (~/.immorterm/config.json)
  ensureGlobalConfig();
  diag('ensureGlobalConfig done');

  // Run migration from .vscode/ to .immorterm/ (idempotent, safe to call always)
  migrateFromVSCodeLayout(workspacePath);
  diag('migrateFromVSCodeLayout done');

  // Migrate GPU appearance settings from VS Code settings → config.json (one-time)
  migrateAppearanceSettings().catch(err =>
    logger.warn('[migration] Appearance migration failed (non-fatal):', err)
  );
  diag('migrateAppearanceSettings scheduled');

  // Check if the user has already made an enable/disable decision for this project.
  // immorterm.enabled is set to true (enabled) or false (disabled) per workspace.
  // If undefined, the user hasn't been asked yet (fresh install or new project).
  const config = vscode.workspace.getConfiguration('immorterm');
  const enabledInspection = config.inspect<boolean>('enabled');
  const enabledValue = enabledInspection?.workspaceValue;
  diag(`enabledValue=${enabledValue}`);

  // Detect Screen availability — sync to avoid yielding the event loop.
  const screenAvailable = detectScreen();
  setScreenAvailable(screenAvailable);
  diag(`detectScreen=${screenAvailable}`);

  // Only extract resources and warn about Screen when enabled.
  // Don't create .immorterm/terminals/ or show warnings before the user opts in.
  let terminalsDir: string;
  let logsDir: string;

  if (enabledValue === true) {
    // Ensure per-project config
    diag('getStableProjectId start');
    let projectId: string;
    try {
      projectId = getStableProjectId(workspacePath);
    } catch (err) {
      diag(`getStableProjectId FAILED: ${err}`);
      // Fallback to folder name if git call fails
      projectId = path.basename(workspacePath).toLowerCase().replace(/[^a-z0-9-]/g, '-');
    }
    diag(`getStableProjectId=${projectId}`);
    ensureProjectConfig(workspacePath, projectId);
    diag('ensureProjectConfig done');

    // One-time migration: sync VS Code settings → config.json
    const projectConfig = readProjectConfig(workspacePath);
    if (projectConfig && projectConfig.enabled === undefined) {
      setEnabledState(workspacePath, true, projectId);

      if (isMemoryEnabled()) {
        setServiceEnabled(workspacePath, 'memory', true, projectId);
      }

      const gatewayEnabled = config.get<boolean>('services.mcpGateway.enabled', false);
      if (gatewayEnabled) {
        setServiceEnabled(workspacePath, 'mcpGateway', true, projectId);
      }

      const theme = config.get<string>('statusBarTheme');
      if (theme) {
        setConfigTheme(workspacePath, theme, projectId);
      }

      logger.info('Migrated VS Code settings → config.json');
    }
    diag('settings migration done');

    diag('extractResources start');
    const extractionResult = extractResources(context, workspaceFolder);
    terminalsDir = extractionResult.terminalsDir;
    logsDir = extractionResult.logsDir;
    diag(`extractResources done: ${extractionResult.extracted.length} extracted, ${extractionResult.skipped.length} skipped`);
    logger.info('Resources extracted:', {
      terminalsDir,
      extracted: extractionResult.extracted,
      skipped: extractionResult.skipped,
    });
  } else {
    // Compute paths without creating directories
    terminalsDir = getTerminalsDir(workspaceFolder);
    logsDir = getLogsDir(workspaceFolder);
    diag('enabledValue not true, skipping extraction');
  }

  // Get project name for storage and session naming
  const projectName = getProjectName(workspaceFolder);
  logger.info('Project name:', projectName);
  diag(`projectName=${projectName}`);

  // Create workspace storage
  diag('WorkspaceStorage start');
  const storage = new WorkspaceStorage(context, projectName);
  diag(`WorkspaceStorage done: ${storage.getTerminalCount()} terminals`);
  logger.debug('WorkspaceStorage initialized with', storage.getTerminalCount(), 'terminals');

  // Create terminal manager
  const terminalManager = new TerminalManager(context, storage);
  diag('TerminalManager done');

  // Create status bar — initialize in background to avoid blocking activation.
  // StatusBar.initialize() triggers async work (fetchVersion, listProjectSessions)
  // that can push RSS over the OOM threshold in heavy workspaces. Fire-and-forget
  // is safe because update() guards against null storage.
  const statusBar = new StatusBar();
  diag('StatusBar created, initializing (non-blocking)...');
  statusBar.initialize(storage, screenAvailable).then(
    () => diag('StatusBar initialized (background)'),
    (err) => diag(`StatusBar init failed (non-critical): ${err}`)
  );

  // Set workspace path for config.json-based service checks
  setMemoryWorkspacePath(workspacePath);
  diag('setMemoryWorkspacePath done');

  // Initialize OpenMemory manager
  initOpenMemoryManager((msg) => logger.debug(msg), context.extensionPath);
  diag('initOpenMemoryManager done');

  // Initialize MCP Gateway manager (pass workspacePath for config.json reads + binary search)
  initGatewayManager((msg) => logger.debug(msg), workspacePath);
  diag('initGatewayManager done');

  // Initialize dashboard API server manager
  initServeManager((msg) => logger.debug(msg));
  diag('initServeManager done');

  if (enabledValue === false) {
    // Explicitly disabled — do nothing. Also strip the terminal-control MCP so
    // its tools stop loading in a project the user turned ImmorTerm off for.
    logger.info('ImmorTerm is disabled for this project. Skipping setup.');
    removeTerminalMCP(workspacePath);
  } else if (enabledValue === true) {
    // Register the terminal-control MCP (screenshot/read_screen/tasks/draw_html)
    // for this ImmorTerm-enabled project. Gated on the master `enabled` flag —
    // NOT the memory sub-toggle — since it's core to ImmorTerm. .mcp.json is
    // project-scoped, so non-enabled projects never get it. Add-if-absent.
    configureTerminalMCP(workspacePath);

    // Already enabled — start memory services if configured
    setTimeout(async () => {
      if (isMemoryEnabled()) {
        await startMemoryServicesPlugAndPlay(workspacePath);
      }
      // Auto-start dashboard API server (fire-and-forget, non-blocking)
      startServe().catch(err => logger.warn('Failed to auto-start serve:', err));
    }, 2000);
  } else {
    // Never asked — prompt the user (deferred to avoid blocking activation)
    setTimeout(async () => {
      const choice = await vscode.window.showInformationMessage(
        'Enable ImmorTerm for this project? Persistent terminals and optional AI memory across sessions.',
        'Enable',
        'Not Now',
      );

      if (choice === 'Enable') {
        // Run the full enable command which handles theme, memory, settings
        await vscode.commands.executeCommand('immorterm.enableForProject');
      } else {
        // User declined — mark as disabled so we don't ask again.
        // Save dirty settings.json first — VS Code refuses config writes on dirty files.
        await saveDirtyWorkspaceSettings();
        // Get a fresh config reference to avoid stale proxy issues.
        const freshConfig = vscode.workspace.getConfiguration('immorterm');
        await freshConfig.update('enabled', false, vscode.ConfigurationTarget.Workspace);

        // Write-through to config.json
        const declineProjectId = getStableProjectId(workspacePath);
        ensureProjectConfig(workspacePath, declineProjectId);
        setEnabledState(workspacePath, false, declineProjectId);

        logger.info('User declined ImmorTerm for this project');

        vscode.window.showInformationMessage(
          'To enable ImmorTerm later, open the Command Palette and run "ImmorTerm: Enable for This Project".',
        );
      }
    }, 1500);
  }

  // ── Config watcher: detect Pro activation and offer gateway ──
  const globalConfigPath = path.join(os.homedir(), '.immorterm', 'config.json');
  let previousLicenseStatus: string | null = null;
  try {
    const raw = fs.readFileSync(globalConfigPath, 'utf-8');
    previousLicenseStatus = JSON.parse(raw)?.license?.status ?? null;
  } catch {
    // No config yet — that's fine
  }

  // Use OS-native fs.watch instead of polling fs.watchFile for config changes
  let configFsWatcher: fs.FSWatcher | null = null;
  let configDebounce: NodeJS.Timeout | null = null;
  const onConfigChange = () => {
    try {
      const raw = fs.readFileSync(globalConfigPath, 'utf-8');
      const currentStatus = JSON.parse(raw)?.license?.status ?? null;

      if (currentStatus === 'active' && previousLicenseStatus !== 'active') {
        logger.info('Detected Pro license activation via config.json watcher');

        vscode.window.showInformationMessage(
          "You're on ImmorTerm Pro! Enable MCP Gateway for faster AI tool routing?",
          'Enable Gateway',
          'Later'
        ).then(async (choice) => {
          if (choice === 'Enable Gateway') {
            const wsConfig = vscode.workspace.getConfiguration('immorterm');
            await wsConfig.update('services.mcpGateway.enabled', true, vscode.ConfigurationTarget.Workspace);

            // Write-through to config.json
            if (workspaceFolder) {
              const pid = getStableProjectId(workspacePath);
              setServiceEnabled(workspacePath, 'mcpGateway', true, pid);
            }

            vscode.window.showInformationMessage('MCP Gateway enabled! Restart VS Code to activate.');
          }
        });
      }
      previousLicenseStatus = currentStatus;
    } catch {
      // Ignore read errors
    }
  };
  try {
    configFsWatcher = fs.watch(globalConfigPath, () => {
      if (configDebounce) clearTimeout(configDebounce);
      configDebounce = setTimeout(onConfigChange, 500);
    });
    configFsWatcher.on('error', () => { /* Config file may not exist yet */ });
  } catch { /* Config file may not exist yet */ }
  context.subscriptions.push({
    dispose: () => {
      if (configDebounce) clearTimeout(configDebounce);
      if (configFsWatcher) configFsWatcher.close();
    },
  });

  logger.info('ImmorTerm initialization complete', {
    screenAvailable,
    projectName,
    terminalCount: storage.getTerminalCount(),
    memoryEnabled: isMemoryEnabled(),
  });

  diag('initializeWorkspace COMPLETE');

  return {
    screenAvailable,
    terminalManager,
    storage,
    statusBar,
    terminalsDir,
    logsDir,
  };
}

/**
 * Helper function to activate the extension
 * This is called from extension.ts activate()
 *
 * @param context The extension context
 * @returns InitializationResult or null if initialization fails
 */
export async function activate(
  context: vscode.ExtensionContext
): Promise<InitializationResult | null> {
  try {
    return await initializeWorkspace(context);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const stack = error instanceof Error ? error.stack : '';
    logger.error('Failed to initialize ImmorTerm:', message);

    // Write to diagnostic log so we can see this outside VS Code
    try {
      const ws = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
      if (ws) {
        const diagPath = path.join(ws, '.immorterm', 'diagnostic.log');
        fs.appendFileSync(diagPath, `[${new Date().toISOString()}] ACTIVATION_ERROR: ${message}\n${stack}\n`);
      }
    } catch { /* ignore */ }

    // Only show error if it's not the expected "no workspace" case
    if (!message.includes('requires a workspace')) {
      await notifications.showError(`Initialization failed: ${message}`);
    }

    return null;
  }
}

export default {
  detectScreen,
  initializeWorkspace,
  activate,
};
